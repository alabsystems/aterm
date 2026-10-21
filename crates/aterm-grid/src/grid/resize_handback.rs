// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Rows hand-back: on a grid that keeps history, rows-only resizes with
//! nothing drawn between them show the same screen at every height they
//! return to, whichever way they went first and however far they overshot.
//!
//! A Native rows-only resize moves such a grid at two edges. A shrink first
//! trims the blank rows below the cursor off the bottom (nothing moves), then
//! demotes top rows into history (every row moves up); a grow reveals the
//! newest history lines at the top (every row moves down), then appends blank
//! rows. Two of those are each other's inverse at the same edge: a grow's
//! reveal takes back a shrink's demote, and a shrink's trim takes back a
//! grow's blank append. The other two are not: a grow after a trim revealed
//! history instead of putting the blank back, and a shrink after a reveal
//! trimmed the blank tail instead of demoting the revealed lines. On a screen
//! with history above it and a blank row under the cursor (a program that
//! cleared and drew a few lines, a short prompt), every such flap left every
//! row and the cursor lower than the program drew them, and a program whose
//! SIGWINCH handler read only the final size never redrew (S2R-F4,
//! ATERM-KI-1015-1).
//!
//! The grid keeps the edge moves the natural resize would not take back
//! ([`Edge`]), with the natural ones made after them, and a resize the other
//! way undoes them newest first:
//!
//! * [`Edge::Trimmed`]: the blank rows the trims took off the bottom, and the
//!   top rows demoted since (`above`). A grow reveals the demoted rows first,
//!   as any grow would, then appends the trimmed rows back as blanks, and
//!   reveals history only for the rest. Each line scrolled into history since
//!   uses one trimmed row up: at the taller height that line would have
//!   landed on the trimmed blank row instead of scrolling, so after it the
//!   reveal is again what the taller screen shows. Output that scrolls nothing
//!   (the program drawing, clearing, moving the cursor) leaves the count
//!   alone, and a grow after it puts back the blank the program never drew on.
//! * [`Edge::Revealed`]: the history lines the grows revealed at the top, and
//!   the blank rows appended since (`above`, once history ran out). A shrink
//!   takes the appended rows first, as any shrink would (trimmed while blank,
//!   else demoted), then demotes the revealed lines back into history, and
//!   trims only for the rest. A line scrolled into history since uses none
//!   up: the taller screen is the shorter one with those lines above it, the
//!   two share their bottom row, and they scroll together. An erase of the
//!   whole screen (ED 2) forgets them, and so does a cursor moved up among
//!   them (CUP, VPA, DECRC, CUU, RI; ED 0 from home is one): the program
//!   draws the new screen from the top, with or without an erase first, so a
//!   shrink under it trims, as on a screen that never grew. So does a shrink
//!   that finds the cursor among them.
//!
//! A grow consumes trims before it reveals afresh and a shrink consumes
//! reveals before it trims afresh, so only one kind is owed at a time. A move
//! of the owed kind made after natural ones (possible only with output between)
//! starts the record again; the older moves are forgotten and resize as an
//! aterm before the hand-back did. A grow reveals history wherever the grid
//! keeps it, the lazy buffer's and the tiered store's once the ring's run out
//! (`Grid::pull_history_into_ring`), so a grid whose history went to its store
//! (a rewrap, a restore) hands back what one whose did not would. It cannot
//! reveal what history no longer keeps: a shrink that demotes more rows than
//! a history cap holds pushes the oldest of them out of it, and a grow back
//! comes back short of them, every row higher (grid SPECS
//! `rows-only-resize-transcript`: the identity holds while retention holds
//! its rows).
//!
//! The bottom-push corner: a shrink past the blank tail with the cursor too
//! near the top to demote rotates the bottom rows into history as its newest
//! lines (`pushed`, in [`Edge::Trimmed`]). A grow puts them back under the
//! screen first, before it reveals demoted rows or appends trimmed blanks, as
//! the screen held them, with their links and images (`restored_bottom`); a
//! shrink that pushes again puts its rows above them, so each comes back
//! where it stood. They are owed only while they are the newest history
//! lines: a line scrolled into history, newest lines taken back out of it
//! (an unscroll, a bottom-anchoring fill), a shrink that trims or demotes
//! (output moved the cursor off the top row the push left it on), a
//! checkpoint, or a grow the resize undo also hands rows back to forgets
//! them, and the rows stay in history as an aterm before the hand-back left
//! them. A resize that finds them past the ring (a rewrap's history goes to
//! the tiered store) brings them back into it first
//! (`Grid::seat_pushed_rows_in_ring`); a ring with no store behind it that
//! evicts one of them forgets them. Under `aterm_resize_undo: 2` they were
//! owed only wholly in the ring, and a rewrap forgot them. While they are owed
//! with the cursor on the top row, a shrink pushes the blank rows a push left
//! at the bottom ([`Seams`]'s gap): they sat between drawn rows, and one
//! shrink over the same rows pushes them with the rest, so a run of shrinks
//! does too. Blank rows past those are the program's (rows erased one by one,
//! a redraw) and are trimmed as on a screen that never pushed, which forgets
//! the pushed rows. An erase of the rows below the cursor (ED 0, ED 2)
//! forgets them as well: they stood below the screen it cleared, and a grow
//! must not put them back under what the program draws next.
//! A shrink under revealed lines that pushes rows the owed part was to take
//! back (appended rows drawn on) owes nothing.
//!
//! A push splits a line where it falls, as one resize there does: the row it
//! pushes first loses its wrap, and so does the screen's new top row, so
//! neither reads as continuing the history line above it ([`Seams`]). A grow
//! that puts the rows back puts the wraps back; rows forgotten keep the cut,
//! a line split in two with all of its text. Under the law before, the pushed
//! rows read as the tail of the line above them, or the head of the screen's.
//!
//! A width change keeps what is owed too, whatever the height does
//! (`Grid::prepare_rewrap`). A rewrap keeps every line, on the screen and in
//! history; what moves is where the screen sits on them. What is owed pins a
//! window: the rows from its top (the screen's top, the history lines trims
//! and demotes put above it, or the revealed lines' end) to its content's
//! end, and its blank rows under the cursor (the screen's, the trimmed ones,
//! less the appended ones). At any height and width the window shows its
//! content and as many of its blank rows as fit, with history above it for
//! any rows left; once its content does not fit, the rows ending at its
//! content's end as far as the cursor allows, and the rest of it pushed off
//! the bottom into history as the bottom-push corner does. A width change
//! lays the rows out that way and owes what puts them back: the window's
//! top above the screen's as demoted rows over the trimmed blank rows it
//! lacks, below it as revealed lines, and its pushed rows, which it first
//! puts back under the screen (`Grid::hand_back_before_resize`) so the
//! rewrap lays them out with the rest. Each size a run of resizes reaches
//! then shows what one resize there from the start shows, whichever sizes
//! came between: a font zoom in then out, a window corner dragged out or in
//! and brought back, a line that rewraps into another number of rows on the
//! way, a cursor homed over drawn rows, and a cursor on a line's tail with
//! its head in history: the width change first brings that head onto the
//! screen, so the line rewraps from its first cell with the cursor on it
//! wherever a run left the screen's top. The window's top is a cell: one
//! that lands inside a row of a rewrapped line is owed with the cells into
//! that row (`HandBack::skew`), so the next width change lays the window out
//! from it, not from the row's first cell, until the program may redraw the
//! row (the cursor moved onto it, the screen erased) or a line scrolls past
//! it with no trims owed. That reads history up to four
//! screens (`Grid::head_walk`); a line whose head is further up is laid out
//! from the screen's top as an aterm before the hand-back laid it out, and
//! what is owed is forgotten, so a width change reads history in proportion
//! to the screen, never to a line's length. A line scrolled into history
//! moves the window's top up a row too, so under this law it counts as one
//! row demoted as well as one trimmed row used up. Under the law before it
//! (`aterm_resize_undo: 2`, [`ResizeLaw::PushedRows`]) a resize on both axes
//! ran as the rows-only resize to the new height, then the width change at
//! that height, which moved rows nothing handed back once a line rewrapped
//! into another number of rows, and cut drawn rows below the cursor; `cast
//! drift` replays the recordings made under it with it (`grid::reflow`
//! keeps its split).
//!
//! The resize undo (`grid::resize_undo`) keeps its own trims and demoted rows
//! on a grid with no history: a trim is counted here only while no undo is
//! open, and a row the undo puts back on top is not a revealed history line,
//! so each is handed back once. A width change with no rewrap (the alternate
//! screen's) and the ConPTY policy (conhost repaints the viewport and its grow
//! reveals nothing) forget what is owed. So does [`Grid::forget_handback`],
//! which also runs the next resize as one pass over both axes, as every resize
//! runs under the law before the hand-back ([`ResizeLaw::Undo`], which `cast
//! drift` replays the recordings made under it with). Only lines scrolled into
//! history use trims up: a move of the row counter that scrolls nothing
//! (history put back in front of the ring, keys raised over a rewrap or a
//! restore) keeps it (`Grid::renumber_rows`). A checkpoint carries it
//! ([`Grid::handback_rows`], [`Grid::handback_above`], [`Grid::seed_handback`]),
//! and the restored grid's grow reveals from the tier its history is restored
//! into, so a rows flap whose halves straddle an update handoff or a restore
//! comes back as the same flap with none between.

use std::borrow::Cow;

use aterm_scrollback::Line;

use super::Grid;
use super::scroll_convert::ScrolledRowExtras;
use super::scrollback_reflow::rewrap_cell_row;
use crate::row_u16;

/// The resize law a grid lays its rows out under: the current one, or an
/// older one that `cast drift` replays a recording made under it with, named
/// by the `aterm_resize_undo` value the recording's header carries
/// ([`ResizeLaw::header_value`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ResizeLaw {
    /// `aterm_resize_undo: 1` (from 2026-09-29): no rows hand-back. What is
    /// owed is forgotten before every resize, which runs as one pass over
    /// both axes.
    Undo,
    /// `aterm_resize_undo: 2` as written until 2026-10-17: the rows hand-back.
    /// A resize on both axes runs as its rows step then its width change, but
    /// a grow that narrows, which runs as one pass; a shrink that pushes rows
    /// off the bottom owes nothing.
    HandBack,
    /// `aterm_resize_undo: 2` as written from 2026-10-17: also the pushed
    /// rows owed back, and a grow that narrows split where its rewrap moves
    /// no row.
    PushedRows,
    /// `aterm_resize_undo: 3` (from 2026-10-19): a width change lays the rows
    /// out where the hand-back puts them, its rewrap's row moves recorded, in
    /// one pass at any height (`Grid::prepare_rewrap`), pushed rows owed
    /// included, from the head of the line the screen's top row continues
    /// (within four screens) and from the cell the window's top starts on
    /// inside its row; a line scrolled into history also counts as a
    /// row demoted; a shrink under owed pushed rows pushes the blank rows a
    /// push left, never trims them, and trims the program's past those; a
    /// push cuts the wraps at its seams and the grow back mends them; an
    /// erase below the cursor forgets the pushed rows.
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

    /// The laws a recording whose header carries `aterm_resize_undo: value`
    /// was made under, the newest first: 2 was written under two. Empty for a
    /// value no aterm wrote (0, or one from a newer aterm).
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

/// The edge moves a resize the other way undoes first (see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Edge {
    /// Nothing owed.
    #[default]
    None,
    /// `rows` blank rows the trims took off the bottom, then `above` top rows
    /// demoted since, then `pushed` bottom rows pushed into history since (the
    /// corner of a cursor too near the top to demote): a grow puts `pushed`
    /// back at the bottom first, then reveals `above`, then appends `rows`.
    Trimmed {
        /// The trimmed blank rows.
        rows: u16,
        /// The rows demoted after them.
        above: u16,
        /// The bottom rows pushed after those: the newest `pushed` history
        /// lines, in the order they stood on the screen, the topmost first.
        pushed: u16,
        /// Where the pushes cut the lines they left (current law only).
        seams: Seams,
    },
    /// `rows` history lines the grows revealed at the top, then `above` blank
    /// rows appended since: a shrink takes `above` first, then demotes `rows`.
    Revealed {
        /// The revealed history lines.
        rows: u16,
        /// The blank rows appended after them.
        above: u16,
    },
}

/// What a bottom push left at its seams, under the current law
/// ([`ResizeLaw::Rewrap`]; see the module doc), owed back with the pushed rows.
///
/// The pushed rows go into history above the screen, out of the place they
/// held under it, so a row that continued the row above it would continue
/// the wrong one: the screen's top row (which continued the history line
/// above the pushed rows), and the pushed rows' first (which continued the
/// screen's last row). Each push takes their wrap off and records it here,
/// and the grow that puts the rows back puts it back, so no row ever
/// continues a line it was not part of. Rows the hand-back forgets stay cut:
/// every line keeps its text, split where the push left it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seams {
    /// The blank rows under the cursor the pushes left at the bottom of the
    /// screen: rows between drawn ones, which a shrink pushes, as one shrink
    /// over the same rows pushes them, rather than trims. Blank rows the
    /// program made since (an erase, a redraw) are past this count.
    pub(super) gap: u16,
    /// The screen's top row continued the history line above the pushed rows.
    pub(super) top: bool,
    /// The pushed rows' first continued the screen's last row.
    pub(super) bottom: bool,
}

/// How a rows-only shrink begins ([`Edge::plan_shrink`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct ShrinkPlan {
    /// The appended blank rows it takes back first, as any shrink would.
    pub(super) natural: usize,
    /// Then the revealed history lines it demotes back.
    pub(super) unreveal: usize,
    /// The cursor sat among the revealed lines: they are forgotten.
    pub(super) forgot: bool,
}

/// What a rows-only shrink did, for [`Edge::after_shrink`].
pub(super) struct Shrunk {
    /// Rows the shrink took off in all.
    pub(super) rows: usize,
    /// How it began.
    pub(super) plan: ShrinkPlan,
    /// Of them, the blank rows trimmed off the bottom.
    pub(super) trimmed: usize,
    /// Of them, the bottom rows pushed into history (the cursor too near the
    /// top to demote them).
    pub(super) pushed: usize,
    /// The trims count toward [`Edge::Trimmed`]: Native, no resize undo open.
    pub(super) counted: bool,
}

/// What a rows-only grow did, for [`Edge::after_grow`].
pub(super) struct Grown {
    /// Rows the grow added past what the resize undo put back.
    pub(super) rows: usize,
    /// Of them, the pushed rows put back at the bottom ([`Edge::plan_grow`]).
    pub(super) unpushed: usize,
    /// Of them, the rows given back as any grow would.
    pub(super) natural: usize,
    /// Of them, the trimmed blank rows appended back.
    pub(super) handed_back: usize,
    /// History lines revealed at the top in all.
    pub(super) revealed: usize,
    /// Of `revealed`, the rows the resize undo put back on top (demoted from a
    /// grid with no history: not history lines).
    pub(super) restored_top: usize,
}

impl Edge {
    /// `rows` trimmed blank rows with `above` rows demoted since, then
    /// `pushed` rows pushed ([`Edge::None`] for no trimmed or pushed rows: a
    /// demote alone is what any grow gives back).
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

    /// The seams the pushed rows owed left (none unless [`Edge::Trimmed`]
    /// with pushes).
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

    /// `self` with the pushed rows' seams set to `seams` (unchanged when no
    /// pushed rows are owed).
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

    /// `rows` revealed history lines with `above` blank rows appended since
    /// ([`Edge::None`] for no rows).
    const fn revealed(rows: u16, above: u16) -> Self {
        if rows == 0 {
            Self::None
        } else {
            Self::Revealed { rows, above }
        }
    }

    /// The trimmed blank rows owed (0 unless [`Edge::Trimmed`]).
    const fn trimmed_rows(self) -> u16 {
        match self {
            Self::Trimmed { rows, .. } => rows,
            Self::None | Self::Revealed { .. } => 0,
        }
    }

    /// The revealed history lines owed (0 unless [`Edge::Revealed`]).
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

    /// What is still owed after `lines` lines scrolled into history: one
    /// trimmed row fewer per line (at the taller height the line would have
    /// landed on it), the reveals all of them (both heights scroll together).
    /// Pushed rows are owed only while they are the newest history lines, so a
    /// line scrolled in after them forgets everything.
    ///
    /// Under the current law each line used up also counts as a row demoted:
    /// the window's top stays on its line while the shorter screen scrolls
    /// past it ([`Grid::prepare_rewrap`] reads it there), and a rewrap can owe
    /// more trimmed rows than the taller height shows, which only the window's
    /// top tells apart. A rows-only grow comes back to the same screen either
    /// way: the line scrolled in is revealed, by the demotes or past them.
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

    /// What is owed with the pushed rows forgotten: the trims and demotes
    /// before them are owed only under them, so nothing.
    const fn without_pushes(self) -> Self {
        match self {
            Self::Trimmed { pushed, .. } if pushed > 0 => Self::None,
            edge => edge,
        }
    }

    /// The owed window's top against the screen's, in rows (above it when
    /// negative): the screen's top when nothing is owed, the demoted rows
    /// above it, or the revealed lines' end below it.
    pub(super) fn top(self) -> isize {
        match self {
            Self::None => 0,
            Self::Trimmed { above, .. } => -isize::try_from(above).unwrap_or(isize::MAX),
            Self::Revealed { rows, .. } => isize::try_from(rows).unwrap_or(isize::MAX),
        }
    }

    /// The natural moves made after the owed ones.
    const fn above(self) -> u16 {
        match self {
            Self::Trimmed { above, .. } | Self::Revealed { above, .. } => above,
            Self::None => 0,
        }
    }

    /// How a shrink of `rows` with the cursor on `cursor_row` begins: the
    /// appended rows it takes back as any shrink would, then the revealed lines
    /// it demotes back. With the cursor among the revealed lines nothing is
    /// demoted back, and they are forgotten.
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

    /// What is owed after the shrink `took`, from `self` owed before it. Its
    /// pushed rows go on top of the ones owed before only when it trimmed and
    /// demoted nothing first (`Grid::adjust_row_count_rows_only` puts them
    /// above those in history); any other shrink after a push forgets the
    /// older moves.
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
        // The trims past the ones the natural part took (the appended blanks),
        // then the rest of the fresh part demoted, then the pushed rows.
        let trimmed_fresh = took.trimmed - natural.min(took.trimmed);
        // Under revealed lines, the pushes can take rows the owed part was to
        // take: an appended row drawn on (not blank, so not trimmed) under a
        // cursor too near the top to demote it. Nothing records them, so the
        // shrink forgets what is owed, as an aterm before the hand-back did.
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
            // With the revealed lines forgotten, what is owed is the fresh
            // part's.
            Self::Revealed { .. } | Self::None => fresh_only,
            Self::Trimmed {
                rows,
                above,
                pushed,
                seams,
            } if pushed > 0 => {
                if trimmed_fresh == 0 && demoted_fresh == 0 {
                    // The caller sets the seams the stacked pushes leave.
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

    /// How a grow of `rows` begins: `(unpushed, natural, handed_back)`, the
    /// pushed rows it puts back at the bottom, then (once they are all back)
    /// the demoted rows it reveals back as any grow would, then the trimmed
    /// blank rows it appends back.
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
        // The natural part's reveals come first; history may run out in it.
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
                    // The caller sets the seams the rows still pushed leave.
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
    /// What is owed after a width change laid the rows out from a
    /// [`RewrapPlan`]: the owed window's top `off` rows below the screen's
    /// top (above it when negative), `tail` blank rows under the cursor
    /// against the window's `base_tail`, and `pushed` of its rows pushed off
    /// the bottom. Below the top, the rows above it are revealed history
    /// lines, and the blank rows past the window's appended ones (history ran
    /// out); above it, the window's blank rows the screen lacks are trimmed,
    /// the rows between demoted, and the pushed rows owed under them.
    /// Anything else (the layout fell short of the plan) owes nothing.
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

/// Where a width change finds the window the hand-back owes back
/// ([`Grid::prepare_rewrap`]), and the blank rows under the cursor it had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RewrapPlan {
    /// The window's top.
    pub(super) top: BaseTop,
    /// The blank rows under the cursor the window had: the ones the screen
    /// has, and the ones trims took, less the ones grows appended.
    pub(super) tail: usize,
}

/// The owed window's top, against the first row the screen rewrap lays out
/// (the screen's top, the head of the line it continued brought onto it,
/// `Grid::reveal_line_head`), and the cell of its row it starts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BaseTop {
    /// This many rows above it at the new width, in history, starting this
    /// many cells into that row.
    Above(usize, u16),
    /// On this source row at or under it (counted from it), this many cells
    /// into it ([`HandBack::skew`]), which the rewrap carries to its new row
    /// and column.
    Below(usize, u16),
}

/// [`Edge`] as of a counter reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HandBack {
    /// What was owed when this was last set.
    edge: Edge,
    /// `absolute_row_counter` when this was last set. Every line scrolled into
    /// history raises the counter by one; a rows-only resize resets this after
    /// it moves the counter itself.
    at: u64,
    /// The next resize runs as one pass over both axes, as an aterm before
    /// the hand-back did ([`Grid::forget_handback`]); the resize spends it.
    one_pass: bool,
    /// What a split's rows step offers the rows its width change frees
    /// ([`Grid::begin_pass`]); the next pass spends it.
    refill: Refill,
    /// Rows the offloaded resize lifted off the top of the screen into its
    /// history job before the pass (the tail of a line whose head is in
    /// history, `Grid::take_boundary_continuation_lines`): the screen's
    /// height is these more than `visible_rows` says until the pass
    /// ([`Grid::settled_rows`]), which spends it.
    lifted: u16,
    /// The plan the offloaded resize made before it took the history away
    /// ([`Grid::offer_plan`]), which the pass spends.
    planned: Option<RewrapPlan>,
    /// What the offloaded resize's re-attach fill settles ([`Grid::settle_fill`]).
    fill: Option<FillOwed>,
    /// The cells into its row at which the owed window's top starts. A width
    /// change can land it inside a row (it stood inside a wrapped line, and
    /// the row its cell lands on starts before it); the next width change
    /// lays the window out from that cell, not the row's first, so a run of
    /// width changes does not walk the window up its line a part of a row at
    /// a time ([`Grid::window_skew`]). Kept only while the record puts the
    /// window's top on the same row and nothing redrew it.
    skew: u16,
}

/// A width change laid the rows out while its history was with the offloaded
/// job: its bottom-anchoring fill runs at re-attach, and what is owed until
/// then counts the rows the fill will pull as far as the blank rows go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FillOwed {
    /// The window's top against the screen's before the fill.
    top: isize,
    /// The window's blank rows.
    base_tail: usize,
    /// The cells into its row at which the window's top starts.
    skew: u16,
}

/// What the rows step of a split resize moved (`Grid::split_rows_step`),
/// offered to the rows its width change frees (a widen that unwraps lines,
/// the tail of a line whose head is in history lifted into the history
/// rewrap) before the bottom-anchoring fill pulls history for the rest
/// ([`Grid::begin_pass`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Refill {
    /// The blank rows the rows step trimmed.
    pub(super) trimmed: u16,
    /// The rows, at the new width, of the lines it demoted after them
    /// (`Grid::demoted_rows_at`): each that opens among the demoted rows,
    /// with its tail still on the screen. One pass kept them on the screen.
    pub(super) back: u16,
    /// Of `trimmed`, the blanks that stood for rows a detach window owed its
    /// re-attach's fill (`Grid::settle_fill_debt_rows`): the synchronous
    /// resize demoted history rows there instead, and the freed rows pull
    /// those back.
    pub(super) owed: u16,
}

impl Refill {
    /// Of `freed` rows the width change freed, how many stay blank on the
    /// screen. The fill pulls the newest history first, so the first freed
    /// rows pull back the lines the rows step demoted, as many rows as they
    /// take at the new width; the next put the trimmed blanks back; the fill
    /// pulls history for the rest.
    pub(super) fn blanks_back(self, freed: usize) -> usize {
        usize::from(self.trimmed).min(freed.saturating_sub(usize::from(self.back)))
    }
}

impl HandBack {
    /// What is still owed with the counter at `counter`: the trims one fewer
    /// per line scrolled into history since, the reveals all of them. A
    /// counter below `at` means something renumbered the rows (a rewrap, a
    /// carried history), and nothing is owed.
    fn owed(self, counter: u64, law: ResizeLaw) -> Edge {
        counter
            .checked_sub(self.at)
            .map_or(Edge::None, |scrolled| self.edge.scrolled(scrolled, law))
    }
}

impl Grid {
    /// What a rows-only resize starts from ([`Edge::None`] under the ConPTY
    /// policy, which hands nothing back).
    pub(super) fn handback_owed(&self, native: bool) -> Edge {
        if native {
            self.storage
                .handback
                .owed(self.storage.absolute_row_counter, self.storage.resize_law)
        } else {
            Edge::None
        }
    }

    /// Whether nothing a later resize reads from the hand-back stands: no rows
    /// owed, and no split pass's offer, lift, plan, fill or skew
    /// ([`Grid::replay_fork`] copies none of them).
    pub(super) fn handback_idle(&self) -> bool {
        let hb = self.storage.handback;
        self.handback_owed(true) == Edge::None
            && HandBack {
                edge: hb.edge,
                at: hb.at,
                ..HandBack::default()
            } == hb
    }

    /// Record `edge` owed as of now, after the rows-only resize that moved the
    /// counter for its own trims and appends.
    pub(super) fn set_handback(&mut self, edge: Edge) {
        self.storage.handback = HandBack {
            edge,
            at: self.storage.absolute_row_counter,
            ..HandBack::default()
        };
    }

    /// Set the row counter for a move that scrolled nothing into history:
    /// lines put back in front of the ring, or keys raised to cover a rewrap
    /// or a restore. What is owed stands, read against the new counter; only
    /// a line scrolled into history uses a trimmed row up.
    pub(super) fn renumber_rows(&mut self, counter: u64) {
        let owed = self
            .storage
            .handback
            .owed(self.storage.absolute_row_counter, self.storage.resize_law);
        let skew = self.window_skew();
        // Outside a lend (which holds the anchors while it renumbers), a
        // counter that moves renumbers every row under them.
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

    /// What a width rewrap keeps of what was owed before it (`owed`, read
    /// before it moved anything): the trims less one per row it pushed into
    /// history, as lines scrolled there use them, and less one per trimmed row
    /// it put back ([`Self::begin_pass`]); set against the counter it left.
    pub(super) fn keep_handback_across_rewrap(&mut self, owed: Edge, used: usize) {
        self.set_handback(
            owed.without_pushes()
                .scrolled(used as u64, self.storage.resize_law),
        );
    }

    /// The newest history lines left the ring with the counter unmoved (an
    /// unscroll put them back on the screen, a bottom-anchoring fill pulled
    /// them into the viewport): the pushed rows owed are no longer the newest
    /// lines, so they are forgotten with the moves owed under them, and a grow
    /// reveals history as an aterm before the hand-back did. Without this it
    /// put back under the screen the lines that are now the newest, which were
    /// never pushed.
    pub(super) fn forget_pushed_rows(&mut self) {
        let edge = self.storage.handback.edge;
        let kept = edge.without_pushes();
        if kept.top() != edge.top() {
            self.storage.handback.skew = 0;
        }
        self.storage.handback.edge = kept;
    }

    /// The cells into its row at which the owed window's top starts
    /// ([`HandBack::skew`]), while what is owed still puts the top on the row
    /// it was set on: with no line scrolled into history since, or, under the
    /// current law, trims owed, which keep the top on its line as lines
    /// scroll past it. 0 otherwise, a top no longer owed among them (the
    /// demoted rows above trims all used up).
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

    /// The width change or rows-only resize just recorded what is owed: the
    /// window's top, `top` rows below the screen's (above it when negative),
    /// starts `skew` cells into its row, kept while what is owed puts the
    /// top there.
    pub(super) fn keep_skew(&mut self, top: isize, skew: u16) {
        if self.handback_owed(true).top() == top {
            self.storage.handback.skew = skew;
        }
    }

    /// The program may redraw the window's top row: the cursor moved up onto
    /// or above it, or the screen was erased. The cell it started on is
    /// forgotten, and the next width change lays the window out from the
    /// row's first cell.
    pub(super) fn forget_skew_at(&mut self, row: u16) {
        let top = self.storage.handback.edge.top();
        if top >= 0 && isize::from(row.cast_signed()) <= top {
            self.storage.handback.skew = 0;
        }
    }

    /// An erase of every row below the cursor (ED 0, ED 2, from wherever the
    /// cursor is), under the current law: at the taller height the rows owed
    /// for a push stood below the screen and the erase took them too, so a
    /// grow must not put them back under what the program just drew. They
    /// are forgotten with the moves owed under them, and stay in history as
    /// an aterm before the hand-back left them. Under the older laws the grow
    /// put them back under the erased screen (`cast drift` replays theirs so).
    pub(super) fn forget_pushed_rows_erased(&mut self) {
        if self.storage.resize_law >= ResizeLaw::Rewrap {
            self.forget_pushed_rows();
        }
    }

    /// A shrink pushed `newer` rows into history above the `older` pushed rows
    /// still owed, which the screen held under them: put the newer rows' ring
    /// extras in front of the older ones', as the rows go. The ring extras end
    /// at the newest history row.
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

    /// Under the current law, take the wraps off at the seams of a bottom push
    /// a rows-only shrink just rotated into the (linearized) ring
    /// ([`Seams`]): its `pushed` rows from ring index `block`, the topmost
    /// first, then, when it went above rows an earlier shrink pushed and still
    /// owes, those (`older`: their count and seams), then the screen. The
    /// older rows' first continued this push's last row, which is above it
    /// again, so its wrap goes back. Returns the seams the rows owed now leave
    /// (their gap is set once the resize is done, [`Self::note_push_gap`]).
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

    /// Under the current law, a rows-only grow just put the top `unpushed` of
    /// the `pushed` rows owed back under the screen (`[rest][screen][them]`
    /// from ring index `hist - pushed`, linearized; `visible` screen rows):
    /// the wraps their seams took off go back where a row again follows the
    /// one it continued, and the rest's new first row, whose predecessor is on
    /// the screen again, loses its own. Returns the seams the rows still owed
    /// leave.
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

    /// After a resize that pushed rows off the bottom or put some back, under
    /// the current law: the blank rows now under the cursor are the ones the
    /// pushes left between drawn rows ([`Seams::gap`]).
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

    /// Take the ring extras of the top `take` rows of the `pushed` rows owed
    /// (the newest `pushed` history rows, topmost first), which a grow puts
    /// back under the screen: `(i, extras)` for the `i`th of them that has
    /// any. The rest of the block keeps its extras.
    pub(super) fn take_unpushed_extras(
        &mut self,
        pushed: usize,
        take: usize,
    ) -> Vec<(usize, Box<ScrolledRowExtras>)> {
        let extras = &mut self.storage.ring_extras;
        let mut block: Vec<_> = (0..pushed).map_while(|_| extras.pop_back()).collect();
        block.reverse();
        // A ring extras shorter than the block holds none for its top rows.
        let missing = pushed - block.len();
        let kept = block.split_off(take.saturating_sub(missing).min(block.len()));
        extras.extend(kept);
        block
            .into_iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|bx| (missing + i, bx)))
            .collect()
    }

    /// Forget what is owed and run the next resize as an aterm before the
    /// hand-back did: a grow reveals history first, a shrink trims first, and
    /// a resize that moves both axes runs as one pass, with no plan
    /// (`Grid::prepare_rewrap`) and not as its rows step then the width change
    /// (`Grid::split_rows_step`). Under that law ([`ResizeLaw::Undo`], which
    /// `cast drift` replays its recordings with) every resize begins so.
    pub fn forget_handback(&mut self) {
        self.storage.handback = HandBack {
            one_pass: true,
            ..HandBack::default()
        };
    }

    /// Drop what is owed: a width change with no rewrap, or under ConPTY,
    /// lays the rows out again with nothing to hand back.
    pub(super) fn drop_handback(&mut self) {
        self.storage.handback = HandBack::default();
    }

    /// Whether this resize runs as one pass ([`Self::forget_handback`]).
    pub(super) fn handback_one_pass(&self) -> bool {
        self.storage.handback.one_pass
    }

    /// Offer the next pass, a split's width change, `refill`
    /// ([`Self::begin_pass`]).
    pub(super) fn offer_refill(&mut self, refill: Refill) {
        self.storage.handback.refill = refill;
    }

    /// A resize pass begins: the one-pass request it answers is spent, and
    /// so is the refill offered to it, which this returns.
    ///
    /// One pass over both axes measured the blank band under the cursor
    /// before the rows moved, so the rows a width change freed absorbed the
    /// shrink, and its bottom-anchoring fill pulled history back only past
    /// that band. Split in two, the rows the width change frees go first to
    /// what the rows step moved ([`Refill`]): the lines it demoted come back
    /// from history, then the blank rows it trimmed, and the fill pulls
    /// history only for the rest, so the split leaves the rows one pass left.
    /// A trimmed row put back is no longer owed
    /// ([`Self::keep_handback_across_rewrap`] uses one trimmed row up for it).
    pub(super) fn begin_pass(&mut self) -> Refill {
        self.storage.handback.one_pass = false;
        self.storage.handback.lifted = 0;
        std::mem::take(&mut self.storage.handback.refill)
    }

    /// The offloaded resize lifted `rows` rows off the top of the screen into
    /// its history job before its pass ([`Self::settled_rows`]).
    pub(super) fn note_rows_lifted(&mut self, rows: usize) {
        self.storage.handback.lifted = row_u16(rows);
    }

    /// The screen's height as the resize under way found it: `visible_rows`,
    /// and the rows an offloaded resize lifted into its history job before
    /// the pass, which its rewrap lays out again as the synchronous resize's
    /// own lift does. A pass that keeps this height moves no rows, so it keeps
    /// what is owed and is not split again.
    pub(super) fn settled_rows(&self) -> u16 {
        self.storage
            .visible_rows
            .saturating_add(self.storage.handback.lifted)
    }

    /// Where a width change to `new_cols` finds the window owed back, read
    /// before anything moves (the current law, [`ResizeLaw::Rewrap`]), with
    /// the head of the line the screen's top row continues brought onto the
    /// screen first ([`Self::reveal_line_head`]), so the rewrap lays that line
    /// out from its head with the cursor on it.
    ///
    /// A width change rewraps the screen and history alike, so each keeps its
    /// lines; what moves is where the screen sits on them. The window a run of
    /// resizes started from is pinned by its top and by the blank rows under
    /// its cursor; at a height and width it shows the rows from its top to its
    /// content's end, then as many of its blank rows as fit, history above it
    /// for any rows left, and once its content does not fit, the rows ending
    /// at its content's end, as far as the cursor allows (a rows-only resize
    /// lays them out the same way, `grid::resize_handback`). Every size a run
    /// of resizes comes back to then shows the same rows, whichever way it
    /// went, a line that rewraps into another number of rows on the way
    /// included. The window's top is the screen's top when nothing is owed,
    /// the history lines trims and demotes put above it, or the revealed
    /// lines' end; its blank rows are the screen's with the trimmed ones, less
    /// the appended ones.
    ///
    /// A line the screen's top row continues was rewrapped in two before
    /// this: its head with the history, its tail with the screen, each from
    /// its own first cell, so it split where the screen's top fell, a row
    /// that ended early in history, wherever a run of resizes had left the
    /// top. With the cursor on the tail the history rewrap could not take the
    /// tail (it carries no cursor), and every resize split it afresh.
    ///
    /// `None`, with nothing moved, when that line's head is more than
    /// [`Self::head_walk`] rows up or a row cannot be read: the width change
    /// then lays the rows out as an aterm before the hand-back did, and what
    /// is owed is forgotten. Rows still owed for a push give the screen's
    /// own window (`Grid::hand_back_before_resize` puts them back first).
    // COST: O((head_walk + rows owed) x cols): what it reads of history is
    // bounded by the screen, never by the history's length.
    pub(super) fn prepare_rewrap(&mut self, new_cols: u16) -> Option<RewrapPlan> {
        // Inside a detach window the blank rows owed to its re-attach
        // (`FillDebt`) stand for history rows above the window, where the
        // synchronous resize has them: not the window's blank rows.
        let tail = self
            .trailing_blank_rows_below_cursor()
            .saturating_sub(self.storage.fill_debt.rows);
        let edge = self.handback_owed(true);
        // The cell of its row the owed top starts on, for the top it is owed at.
        let (skew_top, skew) = (edge.top(), self.window_skew());
        // The window's top, in rows from the screen's top (negative: history),
        // and its blank rows, as what is owed pins them.
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
            // Against the screen's new top, the line head brought onto it.
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

    /// How many history rows a width change reads up to find a line's head
    /// ([`Self::prepare_rewrap`]): four screens. Past that a line is laid out
    /// from the screen's top, so the synchronous half of an offloaded resize
    /// (`Grid::resize_offloading_scrollback`) reads history in proportion to
    /// the screen, never to the line.
    pub(super) fn head_walk(&self) -> usize {
        4 * usize::from(self.storage.visible_rows.max(1))
    }

    /// The history rows of the line the screen's top row continues (0 when
    /// that row starts a line): `None` when its head is more than
    /// [`Self::head_walk`] rows up, past the oldest history line, or on a row
    /// that cannot be read.
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

    /// Bring the newest `rows` history lines, the head of the line the
    /// screen's top row continues, onto the top of the screen, as a rows grow
    /// reveals them, for the width change under way to lay out with the rows
    /// under them ([`Self::prepare_rewrap`]): the cursor, the saved cursor and
    /// the screen's extras move down with their rows, and the revealed rows'
    /// history extras come with them. `false`, with nothing moved, when the
    /// grid cannot bring that many into its ring.
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
        // The deque's tail is the newest ring row; it may hold fewer entries
        // than the ring has rows (older rows without extras).
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
        // The dense rings hold exactly the screen's rows, indexed modulo
        // their count: the rows this grows the screen by have none, and a
        // shift past the last would wrap onto the first, so the top row's
        // ring-held base or true colour would read another row's (`U+FFFD`).
        // Harvest them into the map, read at the live rows, and drop them.
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

    /// Rows, at `new_cols`, from the cell `skew` cells into history row
    /// `from` (-1 the newest line) down to the screen's top, which starts a
    /// line ([`Self::prepare_rewrap`] brought the head of the one it continued
    /// onto the screen), and the column that cell lands on: a start inside a
    /// wrapped line stands on the new row its cell lands on. `None` when
    /// `from`'s line starts more than [`Self::head_walk`] rows above it, or
    /// on a row that cannot be read.
    // COST: O((rows from `from` up to its line's head, at most head_walk,
    // and down to the screen's top) x cols).
    fn rows_down_to(&self, from: isize, skew: u16, new_cols: u16) -> Option<(usize, u16)> {
        let line_at = |at: isize| -> Option<Line> {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_handback_history_reads(1);
            self.try_history_line_rev(at.unsigned_abs().checked_sub(1)?)
                .ok()
                .flatten()
                .map(Cow::into_owned)
        };
        // `from`'s line, from its head.
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

    /// Before a resize, under the current law: a Native width change puts
    /// the rows owed for a push back under the screen at the old width first,
    /// as a rows-only grow does, so its rewrap lays them out with the window
    /// they belong to ([`Self::prepare_rewrap`]) and pushes again only what the
    /// new height leaves no room for. Its rewrap renumbers history, so they
    /// were forgotten in it, and the drawn rows they were stayed in history
    /// above whatever the new height showed. Under the oldest law
    /// ([`ResizeLaw::Undo`]) nothing is owed before any resize.
    ///
    /// Returns what the grow put back, for the resize's one record
    /// ([`Self::merge_handback_step`]); `None` when it ran none.
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
        // As the split rows step did: at the live bottom, with the reader's
        // offset handed back for the width change to place.
        let offset = std::mem::take(&mut self.storage.display_offset);
        self.resize_axes(rows, self.storage.cols, reflow_mode, policy);
        self.storage.display_offset = offset;
        self.storage.last_resize_row_shift = 0;
        Some(self.storage.last_resize_shape)
    }

    /// One record for a resize [`Self::hand_back_before_resize`] began: the
    /// width change's, with the rows the hand-back put back under the screen,
    /// so the resize's record says it gave them back (`restored_bottom`) as a
    /// rows grow does, and the rows the width change pushed off again are its
    /// own `pushed`. The run that pushed them then nets to nothing.
    pub(super) fn merge_handback_step(&mut self, handed: crate::ResizeShape) {
        let shape = &mut self.storage.last_resize_shape;
        shape.restored_bottom = shape.restored_bottom.saturating_add(handed.restored_bottom);
    }

    /// Under the current law, the rows owed for a push come back from
    /// wherever the grid keeps them, as the newest history lines: a rewrap
    /// leaves them in the tiered store when the grid has one
    /// (`Grid::pull_history_into_ring`). Returns whether the ring holds them.
    pub(super) fn seat_pushed_rows_in_ring(&mut self, owed: Edge) -> bool {
        let pushed = usize::from(owed.pushed_rows());
        let ring = self.storage.total_lines - usize::from(self.storage.visible_rows);
        if pushed > ring && self.storage.resize_law >= ResizeLaw::Rewrap {
            self.pull_history_into_ring(pushed - ring);
        }
        pushed <= self.storage.total_lines - usize::from(self.storage.visible_rows)
    }

    /// The grid keeps history the hand-back can move rows through: a ring
    /// that retains lines, or a store past it.
    pub(super) fn keeps_handback_history(&self) -> bool {
        self.storage.max_scrollback > 0 || self.storage.stages_evicted_rows()
    }

    /// The offloaded resize made `plan` before it took the history away: the
    /// pass spends it rather than plan against a history it no longer has.
    pub(super) fn offer_plan(&mut self, plan: RewrapPlan) {
        self.storage.handback.planned = Some(plan);
    }

    /// The plan the offloaded resize offered, spent.
    pub(super) fn take_plan(&mut self) -> Option<RewrapPlan> {
        self.storage.handback.planned.take()
    }

    /// Owe what a width change laid out from a plan: the window's top `top`
    /// rows below the screen's (above it when negative), `skew` cells into
    /// its row, and its `base_tail` blank rows, with the fill done, or `deferred` to the re-attach of an
    /// offloaded resize, which pulls history until `target` blank rows are
    /// left. Until it runs, what is owed counts every row past `target` as
    /// pulled; the re-attach settles it with the rows it did pull.
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

    /// The re-attach of an offloaded resize ran its fill, which pulled
    /// `pulled` rows of history (0 with none left to pull, or no fill): what
    /// the width change owes is where that left the window. Only while
    /// nothing has set what is owed since and no line has scrolled into
    /// history; otherwise what was owed stands.
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
    pub fn resize_law(&self) -> ResizeLaw {
        self.storage.resize_law
    }

    /// Lay the rows out under `law` from the next resize on: an older law for
    /// the replay of a recording made under it (`cast drift`).
    pub fn set_resize_law(&mut self, law: ResizeLaw) {
        self.storage.resize_law = law;
    }

    /// An erase of the whole screen (ED 2): the program draws the new screen
    /// from the top, so the revealed history lines are no longer owed back,
    /// nor the cell of its row the window's top started on.
    pub(super) fn forget_revealed_rows(&mut self) {
        if let Edge::Revealed { .. } = self.storage.handback.edge {
            self.storage.handback = HandBack::default();
        }
        self.forget_skew_at(0);
    }

    /// The cursor just moved up (CUP, HVP, VPA, home, DECRC, CUU, RI): left
    /// among the revealed history lines, it is the program's own to draw on,
    /// a redraw from the top with or without an erase first, so they are no
    /// longer owed back and the shrink trims under its screen, as on one that
    /// never grew. Every move that can go up calls this; the band is the top
    /// `rows` rows, so a cursor that stays on rows the program drew keeps them.
    ///
    /// On or above the row the window's top starts inside, it may redraw that
    /// row too, so the cell it started on is forgotten
    /// ([`Self::forget_skew_at`]).
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

    /// How many blank rows a Native grow would append back before revealing
    /// history afresh (0: none).
    #[must_use]
    pub fn trimmed_rows(&self) -> u16 {
        self.handback_owed(true).trimmed_rows()
    }

    /// How many revealed history lines a Native shrink would demote back
    /// before trimming afresh (0: none).
    #[must_use]
    pub fn revealed_rows(&self) -> u16 {
        self.handback_owed(true).revealed_rows()
    }

    /// How many drawn rows a bottom-push put into history that a grow or a
    /// width change puts back under the screen (0: none). Below the cursor
    /// they are text the screen does not show.
    #[must_use]
    pub fn pushed_rows(&self) -> u16 {
        self.handback_owed(true).pushed_rows()
    }

    /// What is owed, signed, for a checkpoint: positive for trimmed blank rows,
    /// negative for revealed history lines, 0 for nothing. A checkpoint
    /// carries no pushed rows, so a grid that owes them gives 0.
    #[must_use]
    pub fn handback_rows(&self) -> i32 {
        match self.handback_owed(true).without_pushes() {
            Edge::None => 0,
            Edge::Trimmed { rows, .. } => i32::from(rows),
            Edge::Revealed { rows, .. } => -i32::from(rows),
        }
    }

    /// The natural moves made after what is owed, for a checkpoint: rows
    /// demoted after trims, or blank rows appended after reveals.
    #[must_use]
    pub fn handback_above(&self) -> u16 {
        self.handback_owed(true).without_pushes().above()
    }

    /// Take a checkpoint's [`Self::handback_rows`] and [`Self::handback_above`]
    /// as owed from now. A rewrap can move more rows than the screen can
    /// show, so preserve the full stored u16 distances. A corrupt signed
    /// magnitude is bounded to that representation.
    pub fn seed_handback(&mut self, rows: i32, above: u16) {
        let bound = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
        let edge = if rows >= 0 {
            Edge::trimmed(bound(rows.unsigned_abs()), above, 0)
        } else {
            Edge::revealed(bound(rows.unsigned_abs()), above)
        };
        self.set_handback(edge);
    }
}

#[cfg(test)]
mod tests {
    use super::ResizeLaw;
    use crate::grid::Grid;
    use crate::grid::reflow::ResizePolicy;

    /// `history` scrolled lines `h0..`, then the screen cleared and `lines`
    /// drawn from the top (`L0..`), the cursor on the row under them: the
    /// shape a program that clears and draws a few lines leaves.
    fn cleared_with_history(rows: u16, history: u16, lines: u16) -> Grid {
        drawn(Grid::with_scrollback(rows, 10, 100), history, lines)
    }

    /// [`cleared_with_history`] on a grid that keeps history past a ring of
    /// `ring` rows in a tiered store, as the window's terminal does.
    fn tiered_cleared_with_history(rows: u16, ring: usize, history: u16, lines: u16) -> Grid {
        let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
        drawn(
            Grid::with_tiered_scrollback(rows, 10, ring, store),
            history,
            lines,
        )
    }

    /// `history` lines `h0..` scrolled into `grid`'s history, then the screen
    /// cleared and `lines` drawn from the top (`L0..`).
    fn drawn(mut grid: Grid, history: u16, lines: u16) -> Grid {
        for i in 0..history {
            write_line(&mut grid, &format!("h{i}"));
        }
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for i in 0..lines {
            write_line(&mut grid, &format!("L{i}"));
        }
        assert!(grid.scrollback_lines() > 0, "history above the screen");
        grid
    }

    /// `text`, then CR LF.
    fn write_line(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.carriage_return();
        grid.line_feed();
    }

    fn screen(grid: &Grid) -> Vec<String> {
        (0..grid.rows())
            .map(|r| grid.row(r).map(ToString::to_string).unwrap_or_default())
            .collect()
    }

    fn picture(grid: &Grid) -> (Vec<String>, u16, u16, usize) {
        (
            screen(grid),
            grid.cursor_row(),
            grid.cursor_col(),
            grid.scrollback_lines(),
        )
    }

    /// The flap of S2R-F4, and its deeper and stacked forms: the grow hands
    /// the trimmed blanks back, every row and the cursor stay put, and the
    /// history keeps every line.
    #[test]
    fn a_trim_then_a_grow_is_an_identity() {
        for heights in [
            &[7u16, 8][..],
            &[5, 8],
            &[7, 6, 8],
            &[6, 7, 8],
            &[5, 7, 6, 8],
        ] {
            let mut grid = cleared_with_history(8, 12, 2);
            let before = picture(&grid);
            for &h in heights {
                grid.resize(h, 10);
            }
            assert_eq!(picture(&grid), before, "{heights:?}");
            assert_eq!(grid.trimmed_rows(), 0, "{heights:?}: all handed back");
            grid.assert_invariants();
        }

        // The counts: the shrink trims, the grow appends and reveals nothing.
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(7, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.trimmed, grid.trimmed_rows()), (1, 1));
        grid.resize(8, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.appended), (0, 1));
        assert_eq!(grid.take_last_resize_row_shift(), 0, "nothing moved");
    }

    /// The flaps an interactive drag makes: one that grows first (8 -> 9 ->
    /// 8) or overshoots the start on the way back (8 -> 7 -> 9 -> 8). The grow
    /// reveals history at the top, so the shrink back must demote those rows
    /// before it trims the blank tail; trimming first left every row and the
    /// cursor lower than the program drew them, unjudged. Every return to the
    /// starting height of every flap of up to four resizes between 3 and 12
    /// rows is checked.
    #[test]
    fn a_flap_that_grows_first_or_overshoots_is_an_identity() {
        for heights in [
            &[9u16, 8][..],
            &[10, 8],
            &[7, 9, 8],
            &[5, 10, 8],
            &[7, 8, 9, 8],
            &[10, 9, 8],
        ] {
            let mut grid = cleared_with_history(8, 12, 2);
            let before = picture(&grid);
            for &h in heights {
                grid.resize(h, 10);
            }
            assert_eq!(picture(&grid), before, "{heights:?}");
            assert_eq!(grid.handback_rows(), 0, "{heights:?}: all handed back");
            grid.assert_invariants();
        }

        // The counts: the grow reveals and owes it, the shrink demotes it back
        // and trims nothing.
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(9, 10);
        assert_eq!(grid.take_last_resize_shape().revealed, 1);
        assert_eq!((grid.revealed_rows(), grid.handback_rows()), (1, -1));
        grid.resize(8, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.demoted, shrink.trimmed), (1, 0));
        assert_eq!(grid.revealed_rows(), 0);
    }

    /// With nothing drawn between them, rows-only resizes show the same screen
    /// at every height they return to: every run of four resizes between 1 and
    /// 12 rows from 8, on a screen with a blank tail and history (cursor on row
    /// 2), one whose history runs out within a grow (two lines), and one with
    /// no blank tail (cursor on the last row). Runs that come back through the
    /// natural move after an owed one (a grow after trims then demotes, a
    /// shrink after reveals then appends) are among them.
    #[test]
    fn every_height_a_run_returns_to_shows_the_same_screen() {
        // (shape, scrolled lines before the clear, lines drawn after it).
        let shapes = [
            ("blank tail", 12, 2),
            ("short history", 9, 2),
            ("no blank tail", 12, 7),
        ];
        let range = 1u16..=12;
        for (shape, history, lines) in shapes {
            for a in range.clone() {
                for b in range.clone() {
                    for c in range.clone() {
                        let mut grid = cleared_with_history(8, history, lines);
                        let mut seen = std::collections::BTreeMap::new();
                        seen.insert(8u16, picture(&grid));
                        for h in [a, b, c, 8] {
                            grid.resize(h, 10);
                            let now = picture(&grid);
                            let first = seen.entry(h).or_insert_with(|| now.clone());
                            assert_eq!(&now, first, "{shape}: [{a}, {b}, {c}, 8] at {h}");
                        }
                        grid.assert_invariants();
                    }
                }
            }
        }
    }

    /// The natural move after an owed one goes back first. Trims then a demote
    /// (a shrink past the blank tail, 8 -> 3 -> 2): the grow back to 3 reveals
    /// the demoted row, not a trimmed blank. Reveals then appended blanks (a
    /// grow past the history, 8 -> 10 -> 12 with two history lines): the
    /// shrink back to 10 trims the appended blanks, not the revealed lines.
    #[test]
    fn the_natural_move_after_an_owed_one_goes_back_first() {
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(3, 10);
        let at_three = picture(&grid);
        grid.resize(2, 10);
        assert_eq!(grid.take_last_resize_shape().demoted, 1);
        assert_eq!((grid.trimmed_rows(), grid.handback_above()), (5, 1));
        grid.resize(3, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.appended), (1, 0));
        assert_eq!(picture(&grid), at_three);
        assert_eq!((grid.trimmed_rows(), grid.handback_above()), (5, 0));

        let mut grid = cleared_with_history(8, 9, 2);
        grid.resize(10, 10);
        let at_ten = picture(&grid);
        grid.resize(12, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!(
            (grow.revealed, grow.appended),
            (0, 2),
            "the history ran out"
        );
        assert_eq!((grid.revealed_rows(), grid.handback_above()), (2, 2));
        grid.resize(10, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.trimmed, shrink.demoted), (2, 0));
        assert_eq!(picture(&grid), at_ten);
        grid.assert_invariants();
    }

    /// A grow past the starting height hands the trimmed blanks back first,
    /// then reveals history for the rest, and owes the revealed lines.
    #[test]
    fn a_grow_past_the_start_reveals_history_for_the_rest() {
        let mut grid = cleared_with_history(8, 12, 2);
        let history = grid.scrollback_lines();
        grid.resize(7, 10);
        grid.resize(10, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.appended), (2, 1));
        assert_eq!(grid.scrollback_lines(), history - 2);
        assert_eq!(&screen(&grid)[2..4], ["L0", "L1"]);
        assert_eq!((grid.trimmed_rows(), grid.revealed_rows()), (0, 2));
        grid.assert_invariants();
    }

    /// Each line scrolled into history uses one trimmed row up: at the taller
    /// height that line would have landed on the blank row, so the grow
    /// reveals history for it, matching the screen that never resized.
    #[test]
    fn a_line_scrolled_into_history_uses_one_trimmed_row() {
        // Six lines: the cursor on row 6, one blank row under it.
        let mut never = cleared_with_history(8, 12, 6);
        let mut flapped = cleared_with_history(8, 12, 6);
        flapped.resize(7, 10);
        assert_eq!(flapped.trimmed_rows(), 1, "one blank row under the cursor");
        for grid in [&mut never, &mut flapped] {
            write_line(grid, "more");
        }
        assert_eq!(flapped.trimmed_rows(), 0, "the scroll used it");
        flapped.resize(8, 10);
        assert_eq!(flapped.take_last_resize_shape().revealed, 1);
        assert_eq!(picture(&flapped), picture(&never));
        flapped.assert_invariants();
    }

    /// A grow part of the way back after trims and output that scrolled shows
    /// what a screen that stayed at that height shows. A shell: a prompt and
    /// six lines at the top of 50 rows (no history, or after a clear with
    /// history above), shrunk to 10, twenty lines of output, then grown to 12,
    /// 30, 40 or 50. And a grow that found no history to reveal, a shrink past
    /// the start, one line of output, then a grow short of the first height.
    #[test]
    fn a_partial_grow_after_scrolled_output_shows_the_screen_that_stayed() {
        for prior in [0u16, 60] {
            for target in [12u16, 30, 40, 50] {
                let feed = |grid: &mut Grid, part: u8| {
                    if part == 0 {
                        for i in 0..prior {
                            write_line(grid, &format!("old{i}"));
                        }
                        if prior > 0 {
                            grid.erase_screen();
                            grid.set_cursor(0, 0);
                        }
                        for i in 0..6 {
                            write_line(grid, &format!("p{i}"));
                        }
                    } else {
                        for i in 0..20 {
                            write_line(grid, &format!("o{i}"));
                        }
                    }
                };
                let mut flapped = Grid::with_scrollback(50, 10, 1000);
                feed(&mut flapped, 0);
                flapped.resize(10, 10);
                feed(&mut flapped, 1);
                flapped.resize(target, 10);
                let mut stayed = Grid::with_scrollback(target, 10, 1000);
                feed(&mut stayed, 0);
                feed(&mut stayed, 1);
                let at = format!("{prior} lines before, 50 -> 10, 20 lines, -> {target}");
                assert_eq!(screen(&flapped), screen(&stayed), "{at}");
                assert_eq!(flapped.cursor_row(), stayed.cursor_row(), "{at}");
                flapped.assert_invariants();
            }
        }

        let drawn = || {
            let mut grid = Grid::with_scrollback(8, 10, 100);
            for i in 0..4 {
                write_line(&mut grid, &format!("L{i}"));
            }
            grid
        };
        let mut never = drawn();
        let mut flapped = drawn();
        flapped.resize(9, 10);
        flapped.resize(5, 10);
        for grid in [&mut never, &mut flapped] {
            write_line(grid, "X0");
        }
        flapped.resize(8, 10);
        assert_eq!(picture(&flapped), picture(&never), "8 -> 9 -> 5, X0, -> 8");
        flapped.assert_invariants();
    }

    /// A line scrolled into history uses no revealed line up: the taller
    /// screen is the shorter one with the revealed lines above it, so the two
    /// scroll together and the shrink still demotes them, matching the screen
    /// that never resized. Output that draws without scrolling keeps them too.
    #[test]
    fn output_keeps_the_revealed_rows_until_the_shrink() {
        for scroll in [false, true] {
            let mut never = cleared_with_history(8, 12, 2);
            let mut flapped = cleared_with_history(8, 12, 2);
            flapped.resize(9, 10);
            for grid in [&mut never, &mut flapped] {
                write_line(grid, "drawn");
                if scroll {
                    grid.scroll_up(1);
                }
            }
            assert_eq!(flapped.revealed_rows(), 1, "scroll={scroll}");
            flapped.resize(8, 10);
            assert_eq!(picture(&flapped), picture(&never), "scroll={scroll}");
            flapped.assert_invariants();
        }
    }

    /// An erase of the whole screen forgets the revealed lines: the program
    /// draws the new screen from the top, so the shrink trims under it as it
    /// would on a screen that never grew. (The revealed line the erase blanked
    /// is gone from history, as on any terminal whose grow reveals it.)
    #[test]
    fn an_erase_of_the_screen_forgets_the_revealed_rows() {
        let mut never = cleared_with_history(8, 12, 2);
        let mut flapped = cleared_with_history(8, 12, 2);
        flapped.resize(9, 10);
        for grid in [&mut never, &mut flapped] {
            grid.erase_screen();
            grid.set_cursor(0, 0);
            for i in 0..3 {
                write_line(grid, &format!("N{i}"));
            }
        }
        assert_eq!(flapped.revealed_rows(), 0);
        flapped.resize(8, 10);
        let shrink = flapped.take_last_resize_shape();
        assert_eq!((shrink.trimmed, shrink.demoted), (1, 0));
        assert_eq!(screen(&flapped), screen(&never));
        assert_eq!(flapped.cursor_row(), never.cursor_row());
        flapped.assert_invariants();
    }

    /// A shrink that finds the cursor among the revealed lines (the program
    /// moved there to draw) forgets them and trims as it would without them.
    #[test]
    fn a_cursor_among_the_revealed_rows_forgets_them() {
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(10, 10);
        assert_eq!(grid.revealed_rows(), 2);
        grid.set_cursor(1, 0);
        grid.resize(8, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.demoted, shrink.trimmed), (0, 2));
        assert_eq!(grid.handback_rows(), 2, "the trims are owed instead");
        grid.assert_invariants();
    }

    /// Output that scrolls nothing (a frame drawn from the top, a clear)
    /// leaves the trimmed rows owed: the grow still puts the blank back where
    /// the program drew nothing.
    #[test]
    fn output_that_scrolls_nothing_keeps_the_trimmed_rows() {
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(7, 10);
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for c in "redrawn".chars() {
            grid.write_char(c);
        }
        assert_eq!(grid.trimmed_rows(), 1);
        grid.resize(8, 10);
        assert_eq!(grid.row(0).unwrap().to_string(), "redrawn");
        assert_eq!(grid.cursor_row(), 0);
    }

    /// A width change with no rewrap, the ConPTY policy and
    /// [`Grid::forget_handback`] forget both edges: the grow reveals history
    /// first and the shrink trims first, the law before. A Native rewrap keeps
    /// them.
    #[test]
    fn a_width_change_without_a_rewrap_conpty_and_forget_drop_what_is_owed() {
        for grow_first in [false, true] {
            let first = if grow_first { 9 } else { 7 };
            let mut grid = cleared_with_history(8, 12, 2);
            grid.resize(first, 10);
            let owed = (grid.handback_rows(), grid.handback_above());
            assert_ne!(owed.0, 0);
            grid.resize(first, 12);
            assert_eq!(
                (grid.handback_rows(), grid.handback_above()),
                owed,
                "a rewrap keeps it"
            );

            let mut grid = cleared_with_history(8, 12, 2);
            grid.resize(first, 10);
            grid.resize_no_reflow(first, 12);
            assert_eq!(grid.handback_rows(), 0, "a width change with no rewrap");

            let mut grid = cleared_with_history(8, 12, 2);
            grid.resize_with_policy(first, 10, ResizePolicy::ConPty);
            assert_eq!(grid.handback_rows(), 0, "ConPTY");
        }

        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(7, 10);
        grid.forget_handback();
        grid.resize(8, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.appended), (1, 0), "the old law");
        assert_eq!(grid.row(1).unwrap().to_string(), "L0", "every row down one");
        grid.forget_handback();
        grid.resize(7, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.trimmed, shrink.demoted), (1, 0), "the old law");
        assert_eq!(grid.row(1).unwrap().to_string(), "L0", "still down one");
        grid.assert_invariants();
    }

    /// A grow that narrows (taller and narrower at once). Under the current
    /// law it lays the rows out from the window as any resize does, and shows
    /// what the grid never shrunk shows at that size, over a line the narrower
    /// width wraps too. Under `aterm_resize_undo: 2` ([`ResizeLaw::PushedRows`],
    /// which `cast drift` replays those recordings with) it split only where
    /// its rewrap moved no row (`Grid::narrowing_moves_no_row`), and over that
    /// line ran as one pass and forgot what was owed, as an aterm before the
    /// hand-back did, leaving another screen.
    #[test]
    fn a_grow_that_narrows_shows_what_the_grid_never_shrunk_shows() {
        let grid = |law: ResizeLaw, wide: bool| {
            let mut grid = cleared_with_history(8, 12, 2);
            grid.set_resize_law(law);
            if wide {
                write_line(&mut grid, "LLLLLLLLLL");
            }
            grid
        };
        for law in [ResizeLaw::Rewrap, ResizeLaw::PushedRows] {
            for wide in [false, true] {
                let mut never = grid(law, wide);
                never.resize(9, 9);
                let mut kept = grid(law, wide);
                kept.resize(7, 10);
                assert_eq!(kept.trimmed_rows(), 1);
                let mut forgot = grid(law, wide);
                forgot.resize(7, 10);
                forgot.forget_handback();
                kept.resize(9, 9);
                forgot.resize(9, 9);
                kept.assert_invariants();
                let label = format!("{law:?}, wide {wide}");
                if law == ResizeLaw::PushedRows && wide {
                    assert_eq!(picture(&kept), picture(&forgot), "{label}");
                    assert_eq!((kept.handback_rows(), kept.handback_above()), (0, 0));
                    assert_ne!(picture(&kept), picture(&never), "{label}");
                } else {
                    assert_eq!(picture(&kept), picture(&never), "{label}");
                }
            }
        }
    }

    /// A seeded script of resizes on both axes, each with the random word
    /// [`feed`] draws from (`xorshift64`).
    fn script(seed: u64, steps: usize) -> Vec<(u64, u16, u16)> {
        let mut state: u64 = seed;
        (0..steps)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let rows = 3 + u16::try_from((state >> 20) % 10).unwrap();
                let cols = 6 + u16::try_from((state >> 28) % 20).unwrap();
                (state, rows, cols)
            })
            .collect()
    }

    /// Step `step` of a [`script`]: a short or wrapping line, then by `r` an
    /// erase from home, a cursor moved up, a reader scrolled back.
    fn feed(grid: &mut Grid, step: usize, r: u64) {
        // Output lands on the live screen (the terminal pins the view for it).
        grid.scroll_to_bottom();
        let tail = "x".repeat(usize::try_from(r % 25).unwrap());
        write_line(grid, &format!("s{step} {tail}"));
        if (r >> 8).is_multiple_of(7) {
            grid.erase_screen();
            grid.set_cursor(0, 0);
        }
        if (r >> 12).is_multiple_of(5) {
            grid.cursor_up(u16::try_from((r >> 16) % 4).unwrap());
        }
        if (r >> 40).is_multiple_of(6) {
            grid.scroll_display(i32::try_from((r >> 44) % 5).unwrap());
        }
    }

    /// An offloaded resize (`Grid::resize_offloading_scrollback`, the window's
    /// path for a width change) moves the rows and owes back what the
    /// synchronous resize does. Its history goes to the worker before the
    /// pass, the tail of a line whose head is in history lifted with it, and
    /// the pass counted the screen without those rows: a width change read
    /// as a rows change, so it split a second time and forgot what was owed.
    /// Scripts of resizes on both axes on a grid that keeps history past a
    /// short ring, each step from the same grid, under the current law and
    /// under `aterm_resize_undo: 2` ([`ResizeLaw::PushedRows`]), but a grow
    /// that narrows under that one (one pass on both paths, which pull
    /// history back at different times).
    #[test]
    fn an_offloaded_resize_owes_what_the_synchronous_one_does() {
        fn state(grid: &Grid) -> (Vec<String>, u16, u16, usize, super::Edge, u16) {
            let rows = (0..grid.rows())
                .map(|r| {
                    grid.storage
                        .row_at_screen(r)
                        .map(ToString::to_string)
                        .unwrap_or_default()
                })
                .collect();
            (
                rows,
                grid.cursor_row(),
                grid.cursor_col(),
                grid.scrollback_lines(),
                grid.handback_owed(true),
                grid.window_skew(),
            )
        }
        let mut owed = 0;
        for (law, seed) in [ResizeLaw::Rewrap, ResizeLaw::PushedRows]
            .into_iter()
            .flat_map(|law| [0x9e37_79b9_7f4a_7c15, 0x2545_f491_4f6c_dd1d].map(|seed| (law, seed)))
        {
            let steps = script(seed, 50);
            for at in 0..steps.len() {
                let fed = || {
                    let mut grid = tiered_cleared_with_history(8, 6, 12, 2);
                    grid.set_resize_law(law);
                    for (step, &(r, rows, cols)) in steps[..at].iter().enumerate() {
                        feed(&mut grid, step, r);
                        grid.resize(rows, cols);
                    }
                    feed(&mut grid, at, steps[at].0);
                    grid
                };
                let (_, rows, cols) = steps[at];
                let mut sync = fed();
                if law == ResizeLaw::PushedRows && rows > sync.rows() && cols < sync.cols() {
                    continue;
                }
                let mut offloaded = fed();
                sync.resize(rows, cols);
                let mut job = offloaded.resize_offloading_scrollback(rows, cols);
                while let Some(pending) = job {
                    job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
                }
                offloaded.assert_invariants();
                assert_eq!(
                    state(&offloaded),
                    state(&sync),
                    "{law:?} seed {seed:#x} step {at}: {:?}",
                    steps[at]
                );
                owed += usize::from(sync.handback_rows() != 0);
            }
        }
        assert!(owed > 40, "only {owed} steps owed anything");
        // Rows pushed off the bottom under a homed cursor, over blank rows
        // between drawn ones, through runs of resizes on both axes taken
        // offloaded all the way and synchronously all the way.
        let homed = || {
            let mut grid = tiered_cleared_with_history(8, 2, 12, 0);
            for line in ["L0", "LLLLLLLLL1", "L2", "", "L4", "L5", "", "L7"] {
                write_line(&mut grid, line);
            }
            grid.set_cursor(0, 0);
            grid
        };
        let mut pushed = 0;
        for (_, rows, cols) in script(0x8cb9_2ba7_2f3d_8dd7, 40) {
            let (mut sync, mut offloaded) = (homed(), homed());
            for (rows, cols) in [(rows.min(7), cols), (8, 10)] {
                sync.resize(rows, cols);
                let mut job = offloaded.resize_offloading_scrollback(rows, cols);
                while let Some(pending) = job {
                    job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
                }
                offloaded.assert_invariants();
                assert_eq!(state(&offloaded), state(&sync), "homed at {rows}x{cols}");
                pushed += usize::from(sync.handback_owed(true).pushed_rows() != 0);
            }
            assert_eq!(picture(&sync), picture(&homed()), "homed back at 8x10");
        }
        assert!(pushed > 10, "only {pushed} steps owed pushed rows");
        // A line the screen's top row cuts, its head in the tiered store, the
        // cursor on that row: the window's top lands inside a row on the way
        // (`HandBack::skew`), and both paths keep the same cell.
        let split = || {
            let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
            let mut grid = Grid::with_tiered_scrollback(8, 10, 2, store);
            for i in 0..12 {
                write_line(&mut grid, &format!("h{i}"));
            }
            for c in "abcdefghijklmnopqrstuvwxy".chars() {
                grid.write_char_wrap(c);
            }
            for row in ["a", "b", "c", "d", "e", ""] {
                grid.carriage_return();
                grid.line_feed();
                for c in row.chars() {
                    grid.write_char(c);
                }
            }
            grid.set_cursor(0, 1);
            grid
        };
        let mut skewed = 0;
        for (_, rows, cols) in script(0x5851_f42d_4c95_7f2d, 40) {
            let (mut sync, mut offloaded) = (split(), split());
            for (rows, cols) in [(rows, cols), (8, 9), (8, 10)] {
                sync.resize(rows, cols);
                let mut job = offloaded.resize_offloading_scrollback(rows, cols);
                while let Some(pending) = job {
                    job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
                }
                offloaded.assert_invariants();
                assert_eq!(state(&offloaded), state(&sync), "split at {rows}x{cols}");
                skewed += usize::from(sync.window_skew() != 0);
            }
            assert_eq!(picture(&sync), picture(&split()), "split back at 8x10");
        }
        assert!(skewed > 10, "only {skewed} steps kept a cell");
    }

    /// With nothing owed, a resize on both axes moves the rows as one pass
    /// over both axes did whenever nothing is drawn after the cursor, under
    /// the current law (the width change lays the rows out from the screen's
    /// window) and under `aterm_resize_undo: 2` ([`ResizeLaw::PushedRows`]:
    /// the split, `Grid::split_rows_step`): from the same grid, the resize
    /// and one run as one pass ([`Grid::forget_handback`]) leave the same
    /// screen, cursor, reading position and history. Scripts of short and
    /// wrapping lines, erases, cursor moves, a reader scrolled back and
    /// resizes on both axes, checked at every step whose live screen has
    /// nothing drawn at or right of the cursor on its row or on any row below
    /// it (a widen can move a cell right of the cursor onto a row below it).
    /// At the others the resize keeps every line one pass kept, where one pass
    /// cut drawn rows below the cursor off the bottom. Under the old law a
    /// resize shorter and wider than the blank rows under the cursor demotes
    /// lines that take fewer rows at the new width: the freed rows pull back
    /// the rows they take there, not as many as were demoted (`Refill`), or a
    /// line of history comes down where one pass left a blank row (the last
    /// two seeds). The history is never at its retention cap here: the two
    /// steps evict at different widths there.
    #[test]
    fn a_resize_moves_rows_as_one_pass_did_where_nothing_is_drawn_after_the_cursor() {
        /// The retained rows: the history, then the live screen.
        fn lines(grid: &Grid) -> Vec<String> {
            (0..grid.scrollback_lines())
                .map(|i| {
                    grid.get_history_line(i)
                        .map(|l| l.to_string())
                        .unwrap_or_default()
                })
                .chain((0..grid.rows()).map(|r| {
                    grid.storage
                        .row_at_screen(r)
                        .map(ToString::to_string)
                        .unwrap_or_default()
                }))
                .collect()
        }
        /// The retained text as logical lines: a wrapped row continues the one
        /// above it.
        fn logical(grid: &Grid) -> std::collections::BTreeSet<String> {
            let rows = (0..grid.scrollback_lines())
                .filter_map(|i| {
                    grid.get_history_line(i)
                        .map(|l| (l.to_string(), l.is_wrapped()))
                })
                .chain((0..grid.rows()).filter_map(|r| {
                    grid.storage
                        .row_at_screen(r)
                        .map(|row| (row.to_string(), row.is_wrapped()))
                }));
            let mut lines: Vec<String> = Vec::new();
            for (text, continues) in rows {
                match lines.last_mut() {
                    Some(last) if continues => last.push_str(&text),
                    _ => lines.push(text),
                }
            }
            lines
                .into_iter()
                .map(|line| line.trim_end().to_string())
                .collect()
        }
        /// Something is drawn at or right of the cursor on its row, or on a row
        /// below it, on the live screen whatever the reader's position.
        fn drawn_after_the_cursor(grid: &Grid) -> bool {
            let (row, col) = (grid.cursor_row(), grid.cursor_col());
            (row..grid.rows()).any(|r| {
                grid.storage.row_at_screen(r).is_some_and(|cells| {
                    let from = if r == row { col } else { 0 };
                    (from..cells.cols()).any(|c| cells.get(c).is_some_and(|cell| !cell.is_empty()))
                })
            })
        }
        let mut checked = 0;
        let seeds = [
            0x9e37_79b9_7f4a_7c15,
            0x2545_f491_4f6c_dd1d,
            0xd1b5_4a32_d192_ed03,
            0x8cb9_2ba7_2f3d_8dd7,
            0x94d0_49bb_1331_11eb,
            0xbf58_476d_1ce4_e5b9,
            0xfb5a_3753_9839_4d27,
            0x4c0e_d98a_b144_8f08,
        ];
        for (law, seed) in [ResizeLaw::Rewrap, ResizeLaw::PushedRows]
            .into_iter()
            .flat_map(|law| seeds.map(|seed| (law, seed)))
        {
            let steps = script(seed, 60);
            for at in 0..steps.len() {
                let grown = |one_pass: bool| {
                    let mut grid = Grid::with_scrollback(8, 12, 1000);
                    grid.set_resize_law(law);
                    for (step, &(r, rows, cols)) in steps[..at].iter().enumerate() {
                        feed(&mut grid, step, r);
                        grid.resize(rows, cols);
                    }
                    let (r, rows, cols) = steps[at];
                    feed(&mut grid, at, r);
                    let drawn_below = drawn_after_the_cursor(&grid);
                    if one_pass {
                        grid.forget_handback();
                    } else {
                        grid.drop_handback();
                    }
                    grid.resize(rows, cols);
                    grid.assert_invariants();
                    (grid, drawn_below)
                };
                let ((split, drawn_below), (one, _)) = (grown(false), grown(true));
                let label = format!("{law:?} seed {seed:#x} step {at}: {:?}", steps[at]);
                if drawn_below {
                    // Each line one pass kept is somewhere in the resize's
                    // retained text, read across row breaks: where the two
                    // break a line at different rows, the resize may hold it
                    // joined.
                    let kept: String = lines(&split).concat().split_whitespace().collect();
                    for line in logical(&one) {
                        let line: String = line.split_whitespace().collect();
                        assert!(
                            kept.contains(&line),
                            "{label}: the resize lost {line:?}\nresize {:?}\none pass {:?}",
                            lines(&split),
                            lines(&one)
                        );
                    }
                } else {
                    checked += 1;
                    assert_eq!(picture(&split), picture(&one), "{label}");
                    assert_eq!(split.display_offset(), one.display_offset(), "{label}");
                    assert_eq!(lines(&split), lines(&one), "{label}");
                }
            }
        }
        assert!(checked > 360, "only {checked} steps compared whole");
    }

    /// A rewrap that pushes top rows into history owes them as rows demoted
    /// above the trims, and a line scrolled into history after it uses one
    /// trimmed row up and demotes one more: the window's top stays on its
    /// line. Three ten-cell lines over a blank tail, shrunk 8 -> 5 (three
    /// trims of the four blank rows under the cursor), then narrowed to five
    /// columns: the six rows they wrap into and the cursor's need seven, so
    /// two go to history and all four blank rows are owed. The grow back to 8
    /// reveals the two and hands one blank back, which is the screen narrowed
    /// at 8 rows; with a line written at 5 rows first (it scrolls one more
    /// into history), the grow reveals three, which is that screen with the
    /// line written. Owing the scroll as a trim alone, the grow revealed two
    /// and appended a blank, every row one lower than the line left them.
    #[test]
    fn a_rewrap_owes_the_rows_it_pushes_above_the_trims() {
        // A rewrap can demote many more rows than the screen's height.
        // Preserve the side of the screen across the full stored distance,
        // including where a signed 16-bit reinterpretation changes its sign.
        for distance in [32_767_u16, 32_768, 35_980, u16::MAX] {
            let offset = isize::try_from(distance).unwrap();
            let above = super::Edge::after_rewrap(-offset, 0, 2, 0);
            assert_eq!(above.top(), -offset);
            assert_eq!(
                above.scrolled(1, ResizeLaw::Rewrap).top(),
                -isize::try_from(distance.saturating_add(1)).unwrap()
            );
            let below = super::Edge::after_rewrap(offset, 2, 2, 0);
            assert_eq!(below.top(), offset);
        }
        let long = |grid: &mut Grid| {
            for i in 0..12 {
                write_line(grid, &format!("h{i}"));
            }
            grid.erase_screen();
            grid.set_cursor(0, 0);
            for i in 0..3 {
                write_line(grid, &format!("LLLLLLLLL{i}"));
            }
        };
        for scrolled in [false, true] {
            let mut never = Grid::with_scrollback(8, 10, 100);
            long(&mut never);
            never.resize(8, 5);

            let mut grid = Grid::with_scrollback(8, 10, 100);
            long(&mut grid);
            grid.resize(5, 10);
            assert_eq!(grid.trimmed_rows(), 3);
            grid.resize(5, 5);
            assert_eq!(
                (grid.trimmed_rows(), grid.handback_above()),
                (4, 2),
                "two rows pushed above the trims"
            );
            if scrolled {
                write_line(&mut never, "x");
                write_line(&mut grid, "x");
                assert_eq!((grid.trimmed_rows(), grid.handback_above()), (3, 3));
            }
            grid.resize(8, 5);
            assert_eq!(picture(&grid), picture(&never), "scrolled {scrolled}");
            grid.assert_invariants();
        }
    }

    /// A grid with no history keeps its trims and demoted rows in the resize
    /// undo, not here: a flap with output between its halves still appends
    /// blanks as it did, and a row the undo put back on top is not a revealed
    /// history line.
    #[test]
    fn a_grid_without_history_owes_nothing_here() {
        let mut grid = Grid::with_scrollback(8, 10, 0);
        grid.set_cursor(2, 0);
        grid.resize(7, 10);
        assert_eq!(grid.trimmed_rows(), 0);
        assert_eq!(grid.resize_undo_rows(), 0, "a trimmed blank is not a row");
        grid.drop_resize_undo();
        grid.resize(8, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.appended), (0, 1));

        let mut grid = Grid::with_scrollback(4, 10, 0);
        for r in 0..4 {
            grid.set_cursor(r, 0);
            grid.write_char('x');
        }
        grid.resize(3, 10);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape().restored_top, 1);
        assert_eq!(grid.handback_rows(), 0, "the undo's row is not owed");
    }

    /// What is owed rides a checkpoint as a signed count and the natural moves
    /// above it, and a seeded grid hands it back as the live one would; a
    /// value outside the stored distance range is bounded to that range.
    #[test]
    fn the_handback_is_seeded_from_a_signed_count() {
        for path in [&[7u16][..], &[9], &[3, 2]] {
            let flapped = || {
                let mut grid = cleared_with_history(8, 12, 2);
                for &h in path {
                    grid.resize(h, 10);
                }
                grid
            };
            let (owed, above) = (flapped().handback_rows(), flapped().handback_above());
            assert_ne!(owed, 0, "{path:?}");
            let mut seeded = flapped();
            seeded.forget_handback();
            seeded.seed_handback(owed, above);
            assert_eq!(
                (seeded.handback_rows(), seeded.handback_above()),
                (owed, above)
            );
            seeded.resize(8, 10);
            assert_eq!(
                picture(&seeded),
                picture(&cleared_with_history(8, 12, 2)),
                "{path:?}"
            );
        }
        // These are the same full-range edges a width rewrap creates. The
        // checkpoint projection must preserve them even beyond screen height
        // and the old signed-16-bit boundary.
        let mut grid = cleared_with_history(8, 12, 2);
        for distance in [4096_u16, 4097, 32_768, u16::MAX] {
            let offset = isize::try_from(distance).unwrap();
            for edge in [
                super::Edge::after_rewrap(-offset, 0, 2, 0),
                super::Edge::after_rewrap(offset, 2, 2, 0),
            ] {
                grid.set_handback(edge);
                let saved = (grid.handback_rows(), grid.handback_above());
                grid.forget_handback();
                grid.seed_handback(saved.0, saved.1);
                assert_eq!((grid.handback_rows(), grid.handback_above()), saved);
                assert_eq!(grid.handback_owed(true), edge);
            }
        }
        grid.seed_handback(i32::MIN, u16::MAX);
        assert_eq!(grid.revealed_rows(), u16::MAX);
        assert_eq!(grid.handback_above(), u16::MAX);
        grid.seed_handback(0, 3);
        assert_eq!((grid.handback_rows(), grid.handback_above()), (0, 0));
    }

    /// The grids a width-changing flap is checked on: a ring grid, and tiered
    /// grids whose ring holds all of the history, part of it or none.
    fn zoom_shapes(history: u16, lines: u16) -> [(&'static str, Grid); 4] {
        [
            ("ring", cleared_with_history(8, history, lines)),
            (
                "tiered ring 100",
                tiered_cleared_with_history(8, 100, history, lines),
            ),
            (
                "tiered ring 2",
                tiered_cleared_with_history(8, 2, history, lines),
            ),
            (
                "tiered ring 0",
                tiered_cleared_with_history(8, 0, history, lines),
            ),
        ]
    }

    /// S2R-F4 through a width change: a font zoom in then out (Cmd+ then Cmd-),
    /// a zoom out then in, and a window corner dragged and brought back move the
    /// rows and the columns together. On a screen whose lines fit every width
    /// on the way, each such flap must return the screen the program drew, as a
    /// rows-only flap does. The width change forgot what was owed, then trimmed
    /// the blank tail on the shrink and pulled history back down on the grow (its
    /// bottom-anchoring fill), so every row and the cursor ended lower than
    /// drawn, with nothing judged.
    #[test]
    fn a_flap_that_changes_the_width_too_is_an_identity() {
        assert_flaps_are_identities(&[
            &[(7, 9), (8, 10)],
            &[(9, 11), (8, 10)],
            &[(7, 10), (7, 9), (8, 9), (8, 10)],
            &[(6, 8), (7, 9), (8, 10)],
            &[(7, 9), (9, 11), (8, 10)],
            &[(7, 12), (8, 12), (8, 10)],
            &[(9, 11), (6, 8), (10, 12), (8, 10)],
        ]);
    }

    /// S2R-F4 (ATERM-KI-1015-1) through a flap with a half that grows the rows
    /// while it narrows the columns (a window corner dragged down and in, then
    /// back; a zoom out that adds rows and a zoom in), on a screen whose lines
    /// fit every width on the way. That half ran as one pass and forgot what
    /// was owed, so such a flap left the screen lower than drawn (16 of these
    /// 24 runs). It lays the rows out from the window now, as every other
    /// resize does ([`a_grow_that_narrows_shows_what_the_grid_never_shrunk_shows`]).
    #[test]
    fn a_flap_with_a_taller_and_narrower_half_is_an_identity() {
        assert_flaps_are_identities(&[&[(9, 9), (8, 10)], &[(5, 12), (8, 10)]]);
    }

    /// Each of `paths`, from each [`zoom_shapes`] grid of three histories,
    /// returns the picture it started from.
    fn assert_flaps_are_identities(paths: &[&[(u16, u16)]]) {
        for (history, lines) in [(12, 2), (9, 2), (12, 7)] {
            for path in paths {
                for (shape, mut grid) in zoom_shapes(history, lines) {
                    let before = picture(&grid);
                    for &(rows, cols) in *path {
                        grid.resize(rows, cols);
                    }
                    assert_eq!(
                        picture(&grid),
                        before,
                        "{shape}, history {history}, {lines} lines: {path:?}"
                    );
                    grid.assert_invariants();
                }
            }
        }
    }

    /// THE BOTTOM-PUSH CORNER (S2R-F4, ATERM-KI-1015-1): a shrink past the
    /// blank rows under a cursor too near the top to demote pushes the bottom
    /// rows into history, and the grow back puts them back under the screen,
    /// the topmost first, before it reveals or appends anything. The grid
    /// forgot them, so the grow revealed them at the top, out of order (live,
    /// `seq 1 120` then `CSI H` at 144x50 flapped to 48 rows: `71, 120, 72,
    /// ...`, the cursor two rows down, `displaced=+2`, only `cast drift`
    /// seeing it). Every run of four resizes between 1 and 12 rows from 8
    /// shows the same screen and the same history at every height it returns
    /// to, on a full screen over a blank row with the cursor homed (a program
    /// that printed past the screen, then `CSI H`), with it on row 2 (two rows
    /// demoted before the push), and on a full screen with no blank row.
    #[test]
    fn a_bottom_push_flap_is_an_identity() {
        let numbered = |cursor: u16, blank_row: bool| {
            let mut grid = Grid::with_scrollback(8, 10, 100);
            for i in 1..=19 {
                write_line(&mut grid, &i.to_string());
            }
            if blank_row {
                write_line(&mut grid, "20");
            } else {
                grid.write_char('2');
                grid.write_char('0');
            }
            grid.set_cursor(cursor, 0);
            grid
        };
        let state = |grid: &Grid| {
            let history: Vec<String> = (0..grid.scrollback_lines())
                .map(|i| {
                    grid.get_history_line(i)
                        .map(|l| l.to_string())
                        .unwrap_or_default()
                })
                .collect();
            (picture(grid), history)
        };

        // The counts: the shrink trims the blank row and pushes the bottom
        // one; the grow puts that one back and the blank under it.
        let mut grid = numbered(0, true);
        grid.resize(6, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.trimmed, shrink.pushed), (1, 1));
        grid.resize(8, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!(
            (grow.restored_bottom, grow.revealed, grow.appended),
            (1, 0, 1)
        );
        assert_eq!(state(&grid), state(&numbered(0, true)));

        // The pushed rows' cell extras ride with them, into history and back
        // to the rows they left, whether one shrink pushed both or a run of
        // one-row shrinks did, and whether one grow or two hands them back.
        let linked = || {
            let mut grid = numbered(0, true);
            for row in [5u16, 6] {
                let url: std::sync::Arc<str> = std::sync::Arc::from(format!("https://row/{row}"));
                grid.extras_mut()
                    .get_or_create(crate::grid::CellCoord::new(row, 0))
                    .set_hyperlink(Some(url));
            }
            grid
        };
        let link = |grid: &Grid, row: u16| {
            grid.extras()
                .get(crate::grid::CellCoord::new(row, 0))
                .and_then(|e| e.hyperlink())
                .map(ToString::to_string)
        };
        for (down, up) in [(&[5u16][..], &[8u16][..]), (&[7, 6, 5], &[6, 7, 8])] {
            let mut grid = linked();
            for &h in down {
                grid.resize(h, 10);
            }
            let newest: Vec<String> = (0..2)
                .map(|i| grid.history_line_rev(1 - i).unwrap().to_string())
                .collect();
            assert_eq!(
                newest,
                ["19", "20"],
                "{down:?}: pushed in the screen's order"
            );
            for &h in up {
                grid.resize(h, 10);
            }
            assert_eq!(state(&grid), state(&numbered(0, true)), "{down:?} {up:?}");
            for row in [5u16, 6] {
                assert_eq!(
                    link(&grid, row),
                    Some(format!("https://row/{row}")),
                    "{down:?}"
                );
            }
            grid.assert_invariants();
        }

        // Blank rows between the drawn ones under a homed cursor: a push
        // leaves one at the bottom, and the next shrink pushes it above the
        // pushed rows, as one shrink over both pushes them. Trimmed, it forgot
        // the pushed rows, and the grow back revealed them at the top (`A`,
        // blank, `B` homed, 3 -> 2 -> 1 -> 3 rows read `B, A`).
        let gapped = || {
            let mut grid = Grid::with_scrollback(3, 10, 100);
            for line in ["h0", "h1", "A", ""] {
                write_line(&mut grid, line);
            }
            grid.write_char('B');
            grid.set_cursor(0, 0);
            grid
        };
        let mut grid = gapped();
        for h in [2, 1, 3] {
            grid.resize(h, 10);
        }
        assert_eq!(state(&grid), state(&gapped()));
        let gaps = || {
            let mut grid = Grid::with_scrollback(8, 10, 100);
            for i in 1..=20 {
                let line = if i % 3 == 0 {
                    String::new()
                } else {
                    i.to_string()
                };
                write_line(&mut grid, &line);
            }
            grid.write_char('x');
            grid.set_cursor(0, 0);
            grid
        };

        let range = 1u16..=12;
        let shapes: [(&str, &dyn Fn() -> Grid); 4] = [
            ("homed", &|| numbered(0, true)),
            ("cursor on row 2", &|| numbered(2, true)),
            ("no blank row", &|| numbered(0, false)),
            ("blank rows between, homed", &gaps),
        ];
        for (shape, start) in shapes {
            for a in range.clone() {
                for b in range.clone() {
                    for c in range.clone() {
                        let mut grid = start();
                        let mut seen = std::collections::BTreeMap::new();
                        seen.insert(8u16, state(&grid));
                        for h in [a, b, c, 8] {
                            grid.resize(h, 10);
                            let now = state(&grid);
                            let first = seen.entry(h).or_insert_with(|| now.clone());
                            assert_eq!(&now, first, "{shape}: [{a}, {b}, {c}, 8] at {h}");
                        }
                        grid.assert_invariants();
                    }
                }
            }
        }
    }

    /// A shrink under revealed lines whose appended rows were drawn on, with
    /// the cursor too near the top to demote them, pushes the rows the owed
    /// part was to take back. It forgets what is owed, as an aterm before the
    /// hand-back did, and keeps every line. Counting those pushes as fresh
    /// underflowed: a panic in a debug build, a bogus count in a release one.
    #[test]
    fn a_push_of_drawn_on_appended_rows_forgets_what_is_owed() {
        let mut grid = Grid::with_scrollback(3, 10, 100);
        for s in ["h0", "a", "b"] {
            write_line(&mut grid, s);
        }
        grid.write_char('c');
        grid.resize(6, 10);
        assert_eq!(grid.revealed_rows(), 1, "h0 revealed, two blanks appended");
        for s in ["d", "e"] {
            grid.carriage_return();
            grid.line_feed();
            grid.write_char(s.chars().next().unwrap());
        }
        grid.set_cursor(1, 0);
        assert_eq!(grid.revealed_rows(), 1, "the cursor stays below h0");
        grid.resize(1, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.trimmed, shrink.demoted, shrink.pushed), (0, 1, 4));
        assert_eq!((grid.trimmed_rows(), grid.revealed_rows()), (0, 0));
        grid.resize(6, 10);
        let mut lines: Vec<String> = (0..grid.scrollback_lines())
            .filter_map(|i| grid.get_history_line(i).map(|l| l.to_string()))
            .chain(screen(&grid))
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        lines.sort();
        assert_eq!(lines, ["a", "b", "c", "d", "e", "h0"]);
        grid.assert_invariants();
    }

    /// An unscroll (`CSI n + T`) takes the newest history lines back onto the
    /// screen with the row counter unmoved, so the pushed rows owed are no
    /// longer the newest lines: they are forgotten, and the grow reveals
    /// history at the top as an aterm before the hand-back did. Owed still, it
    /// put back under the screen a line that was never pushed (`2` here).
    #[test]
    fn an_unscroll_forgets_the_pushed_rows() {
        let mut grid = Grid::with_scrollback(4, 10, 100);
        for i in 1..=5 {
            write_line(&mut grid, &i.to_string());
        }
        grid.write_char('6');
        grid.set_cursor(0, 0);
        grid.resize(2, 10);
        assert_eq!(grid.take_last_resize_shape().pushed, 2, "5 and 6 pushed");
        assert_eq!(grid.unscroll_from_scrollback(1), 1);
        assert_eq!(screen(&grid), ["6", "3"]);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape().restored_bottom, 0);
        assert_eq!(screen(&grid), ["2", "5", "6", "3"]);
        assert_eq!(grid.scrollback_lines(), 1);
        grid.assert_invariants();
    }

    /// An erase of the rows below the cursor (ED 2, or ED 0 from home) after
    /// a push forgets the rows owed for it: they stood below the screen the
    /// erase cleared, so a grow reveals them at the top with the rest of
    /// history, under the current law, and a shrink trims the blank rows the
    /// erase left. Owed still, as under the law before, the grow put them back
    /// under what the program drew after the erase, and a shrink that kept
    /// the pushed rows' order pushed the erased blanks into history.
    #[test]
    fn an_erase_forgets_the_pushed_rows() {
        let history = |grid: &Grid| -> Vec<String> {
            (0..grid.scrollback_lines())
                .filter_map(|i| grid.get_history_line(i).map(|l| l.to_string()))
                .collect()
        };
        let erased = |law: ResizeLaw, ed2: bool| {
            let mut grid = Grid::with_scrollback(8, 10, 100);
            grid.set_resize_law(law);
            for i in 0..10 {
                write_line(&mut grid, &format!("h{i}"));
            }
            grid.erase_screen();
            grid.set_cursor(0, 0);
            for row in ["A", "B", "C", "D", "E", "F", "G"] {
                write_line(&mut grid, row);
            }
            grid.write_char('H');
            grid.set_cursor(0, 0);
            grid.resize(6, 10);
            assert_eq!(grid.take_last_resize_shape().pushed, 2, "G and H pushed");
            if ed2 {
                grid.erase_screen();
            } else {
                grid.erase_to_end_of_screen();
            }
            grid.write_char('n');
            grid.set_cursor(0, 0);
            grid
        };
        let old = ["h0", "h1", "h2"];
        for ed2 in [true, false] {
            for heights in [&[8u16][..], &[4, 8], &[3, 5, 8]] {
                let mut grid = erased(ResizeLaw::Rewrap, ed2);
                for &h in heights {
                    grid.resize(h, 10);
                    assert!(
                        history(&grid).iter().all(|line| !line.trim().is_empty()),
                        "ED 2 {ed2}, {heights:?} at {h}: a blank row archived: {:?}",
                        history(&grid)
                    );
                }
                assert_eq!(
                    screen(&grid),
                    ["G", "H", "n", "", "", "", "", ""],
                    "ED 2 {ed2}, {heights:?}"
                );
                assert_eq!(history(&grid), old, "ED 2 {ed2}, {heights:?}");
                grid.assert_invariants();
            }
            let mut grid = erased(ResizeLaw::PushedRows, ed2);
            grid.resize(8, 10);
            assert_eq!(
                screen(&grid),
                ["n", "", "", "", "", "", "G", "H"],
                "ED 2 {ed2}: the law before"
            );
        }
        // Rows erased one by one (EL) leave more blank rows under the cursor
        // than the push did (`Seams::gap`): they are the program's, trimmed
        // as on a screen that never pushed, and the pushed rows are forgotten
        // with them, as after an erase of the screen.
        let mut grid = erased(ResizeLaw::Rewrap, true);
        grid.resize(8, 10);
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for row in ["A", "B", "C", "D", "E", "F", "G"] {
            write_line(&mut grid, row);
        }
        grid.write_char('H');
        grid.set_cursor(0, 0);
        grid.resize(6, 10);
        for row in 0..6 {
            grid.set_cursor(row, 0);
            grid.erase_line();
        }
        grid.set_cursor(0, 0);
        grid.write_char('n');
        grid.set_cursor(0, 0);
        grid.resize(4, 10);
        assert!(
            history(&grid).iter().all(|line| !line.trim().is_empty()),
            "EL: a blank row archived: {:?}",
            history(&grid)
        );
        grid.resize(8, 10);
        assert_eq!(screen(&grid), ["G", "H", "n", "", "", "", "", ""], "EL");
        grid.assert_invariants();
    }

    /// A rows-only shrink that pushes rows off the bottom cuts a line where
    /// the push splits it ([`Seams`]), under the current law: the history row
    /// a pushed continuation would join, and the pushed rows a screen's top
    /// row would join (the cursor on a line's tail, its head in history),
    /// are never spliced into that line. The grow that puts the rows back
    /// mends both seams, whichever way a run of shrinks and grows went. Under
    /// the law before, the pushed rows read as part of the line above them.
    #[test]
    fn a_push_cuts_a_line_at_its_seams_and_the_grow_back_mends_it() {
        /// The logical lines, oldest first, and every row's wrap.
        fn lines(grid: &Grid) -> (Vec<String>, Vec<bool>) {
            let rows: Vec<(String, bool)> = (0..grid.scrollback_lines())
                .filter_map(|i| {
                    grid.get_history_line(i)
                        .map(|l| (l.to_string(), l.is_wrapped()))
                })
                .chain(
                    (0..grid.rows())
                        .filter_map(|r| grid.row(r).map(|row| (row.to_string(), row.is_wrapped()))),
                )
                .collect();
            let mut lines: Vec<String> = Vec::new();
            for (text, continues) in &rows {
                match lines.last_mut() {
                    Some(last) if *continues => last.push_str(text),
                    _ => lines.push(text.clone()),
                }
            }
            let lines = lines
                .into_iter()
                .map(|line| line.trim_end().to_string())
                .filter(|line| !line.is_empty())
                .collect();
            (lines, rows.into_iter().map(|(_, wrap)| wrap).collect())
        }
        let write = |grid: &mut Grid, line: &str| {
            for c in line.chars() {
                grid.write_char_wrap(c);
            }
        };
        // A line wrapped across the bottom edge under a homed cursor.
        let straddle = |law: ResizeLaw| {
            let mut grid = Grid::with_scrollback(6, 10, 100);
            grid.set_resize_law(law);
            for i in 0..8 {
                write_line(&mut grid, &format!("h{i}"));
            }
            grid.erase_screen();
            grid.set_cursor(0, 0);
            for row in ["a", "b", "c"] {
                write_line(&mut grid, row);
            }
            write(&mut grid, &"L".repeat(25));
            grid.set_cursor(0, 0);
            grid
        };
        // The cursor on a line's second row, its head in history.
        let on_the_tail = |law: ResizeLaw| {
            let mut grid = Grid::with_scrollback(6, 10, 100);
            grid.set_resize_law(law);
            for i in 0..4 {
                write_line(&mut grid, &format!("h{i}"));
            }
            write(&mut grid, &"L".repeat(25));
            for row in ["", "a", "b", "c"] {
                grid.carriage_return();
                grid.line_feed();
                write(&mut grid, row);
            }
            grid.set_cursor(0, 1);
            assert!(grid.row(0).is_some_and(|row| row.is_wrapped()));
            grid
        };
        let shapes: [(&str, &dyn Fn(ResizeLaw) -> Grid); 2] = [
            ("straddling the bottom", &straddle),
            ("the cursor on its tail", &on_the_tail),
        ];
        // No kept line holds both a line's `L`s and another line's text.
        let spliced = |grid: &Grid| {
            lines(grid)
                .0
                .iter()
                .any(|line| line.contains('L') && line.contains(|c: char| c != 'L'))
        };
        for (shape, start) in shapes {
            let before = lines(&start(ResizeLaw::Rewrap));
            for run in [&[4u16, 6][..], &[5, 4, 6], &[4, 5, 6], &[3, 5, 4, 6]] {
                let mut grid = start(ResizeLaw::Rewrap);
                for &h in run {
                    grid.resize(h, 10);
                    assert!(
                        !spliced(&grid),
                        "{shape}: {run:?} at {h}: {:?}",
                        lines(&grid)
                    );
                    grid.assert_invariants();
                }
                assert_eq!(lines(&grid), before, "{shape}: {run:?}");
            }
            let mut grid = start(ResizeLaw::PushedRows);
            grid.resize(4, 10);
            assert!(spliced(&grid), "{shape}: the law before splices");
        }
    }

    /// A zoom flap over a line the screen's top row cuts (its head scrolled
    /// into history, the cursor on that top row, a blank row under the drawn
    /// ones) comes back to the screen it left. A width change lands the
    /// window's top inside a row of the rewrapped line, and the next one lays
    /// the window out from that cell (`HandBack::skew`). From the row's first
    /// cell, each width change walked the window up its line by part of a
    /// row, and 10 -> 9 -> 10 columns showed the line's head on top and cut
    /// the blank row (lab, 144x50: `cast drift` desync, 48 rows moved).
    /// The cell is forgotten where the program may redraw that row: the
    /// cursor moved onto it, the screen erased, a line scrolled past it.
    #[test]
    fn a_zoom_flap_over_a_line_the_screen_top_cuts_is_an_identity() {
        let start = || {
            let mut grid = Grid::with_scrollback(8, 10, 100);
            for i in 0..4 {
                write_line(&mut grid, &format!("h{i}"));
            }
            for c in "abcdefghijklmnopqrstuvwxy".chars() {
                grid.write_char_wrap(c);
            }
            for row in ["a", "b", "c", "d", "e", ""] {
                grid.carriage_return();
                grid.line_feed();
                for c in row.chars() {
                    grid.write_char(c);
                }
            }
            grid.set_cursor(0, 1);
            assert_eq!(screen(&grid)[0], "klmnopqrst");
            grid
        };
        for run in [
            &[(8u16, 9u16), (8, 10)][..],
            &[(8, 8), (8, 9), (8, 10)],
            &[(7, 9), (6, 8), (7, 9), (8, 10)],
            &[(8, 6), (5, 7), (8, 10)],
        ] {
            let mut grid = start();
            for &(rows, cols) in run {
                grid.resize(rows, cols);
                let mut straight = start();
                straight.resize(rows, cols);
                assert_eq!(
                    picture(&grid),
                    picture(&straight),
                    "{run:?} at {rows}x{cols}"
                );
                grid.assert_invariants();
            }
            assert_eq!(picture(&grid), picture(&start()), "{run:?}");
        }
        // The cell is kept only while the row is the one it was set on.
        let skewed = || {
            let mut grid = start();
            grid.resize(8, 9);
            assert_eq!(grid.window_skew(), 1, "10 cells in: one into `jklmnopqr`");
            grid
        };
        let mut grid = skewed();
        grid.set_cursor(3, 0);
        assert_eq!(grid.window_skew(), 1, "the cursor below the top keeps it");
        grid.set_cursor(0, 0);
        assert_eq!(grid.window_skew(), 0, "the cursor on the top row");
        let mut grid = skewed();
        grid.erase_screen();
        assert_eq!(grid.window_skew(), 0, "the screen erased");
        let mut grid = skewed();
        grid.set_cursor(7, 0);
        write_line(&mut grid, "x");
        assert_eq!(grid.window_skew(), 0, "a line scrolled past it");
    }

    /// The record of a width change that runs the rows hand-back's grow
    /// first counts what that grow put back with what the rewrap pushed again
    /// ([`Grid::merge_handback_step`]): the rows owed for a push go back
    /// under the screen and a rewrap at the same height pushes them again,
    /// two of each, and the grow back puts them under the screen for good.
    /// Uncounted, the rows the rewrap pushed read as none and the rows put
    /// back as none, so a ledger netted pushes it never saw.
    #[test]
    fn a_width_change_counts_the_pushed_rows_it_put_back_and_pushed() {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        for i in 0..4 {
            write_line(&mut grid, &format!("h{i}"));
        }
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for row in ["A", "B", "C", "D", "E", "F", "G"] {
            write_line(&mut grid, row);
        }
        grid.write_char('H');
        grid.set_cursor(0, 0);
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape().pushed, 2);
        grid.resize(6, 9);
        let narrow = grid.take_last_resize_shape();
        assert_eq!(
            (narrow.restored_bottom, narrow.pushed, narrow.reflowed),
            (2, 2, true)
        );
        assert_eq!(screen(&grid), ["A", "B", "C", "D", "E", "F"]);
        grid.resize(8, 10);
        let back = grid.take_last_resize_shape();
        assert_eq!((back.restored_bottom, back.pushed), (2, 0));
        assert_eq!(screen(&grid), ["A", "B", "C", "D", "E", "F", "G", "H"]);
        grid.assert_invariants();
    }

    /// [`every_height_a_run_returns_to_shows_the_same_screen`] with the width
    /// following the height, as a font zoom moves both: one column fewer below
    /// 8 rows, one more above. Every run of four resizes between 1 and 12 rows
    /// shows the same screen at every size it returns to.
    #[test]
    fn every_size_a_zoom_run_returns_to_shows_the_same_screen() {
        let cols = |rows: u16| match rows.cmp(&8) {
            std::cmp::Ordering::Less => 9,
            std::cmp::Ordering::Equal => 10,
            std::cmp::Ordering::Greater => 11,
        };
        let range = 1u16..=12;
        for (history, lines) in [(12, 2), (9, 2), (12, 7)] {
            for a in range.clone() {
                for b in range.clone() {
                    for c in range.clone() {
                        let mut grid = cleared_with_history(8, history, lines);
                        let mut seen = std::collections::BTreeMap::new();
                        seen.insert(8u16, picture(&grid));
                        for h in [a, b, c, 8] {
                            grid.resize(h, cols(h));
                            let now = picture(&grid);
                            let first = seen.entry(h).or_insert_with(|| now.clone());
                            assert_eq!(
                                &now, first,
                                "history {history}, {lines} lines: [{a}, {b}, {c}, 8] at {h}"
                            );
                        }
                        grid.assert_invariants();
                    }
                }
            }
        }
    }

    /// THE REWRAP CORNER (S2R-F4, ATERM-KI-1015-1): a line that rewraps into
    /// another number of rows on the way. Under the current law
    /// ([`ResizeLaw::Rewrap`]) a resize lays the rows out from the window the
    /// run started from (`Grid::prepare_rewrap`), so every size shows what the
    /// grid that never resized shows resized straight there, whichever sizes
    /// came between and whatever lines scrolled into history on the way: the
    /// same screen, cursor and history count, and the same logical lines.
    /// Every line keeps its text, and no row ever continues a line it was not
    /// part of: a push off the bottom splits a line at its seams, where one
    /// resize there splits it too (`Seams`), never splices another into it.
    /// Seeded runs of five resizes between 3 and 10 rows and 6 and 14
    /// columns, a short line written after some of them, on screens written
    /// as a program writes them (autowrap on, so a long line soft-wraps):
    /// a line that wraps below 10 columns, a wrapped line, wrapped history
    /// lines, a full screen, a prompt, a cursor homed or mid-screen over drawn
    /// rows (blank rows between them), and a cursor on a wrapped line's second
    /// row with drawn rows under it, on a ring grid and a tiered one. The
    /// rewrap moved rows nothing handed back, cut drawn rows below the cursor,
    /// split a line whose head was in history at the screen's top on every
    /// width change with the cursor on its tail, and spliced the rows a push
    /// put above that tail into the line: under the law before it
    /// (`aterm_resize_undo: 2`) most of these runs fail, the census's negative
    /// control.
    #[test]
    fn every_size_a_run_reaches_shows_what_one_resize_there_shows() {
        /// The retained text as logical lines, sorted: a wrapped row
        /// continues the one above it.
        fn kept_lines(grid: &Grid) -> Vec<String> {
            let rows = (0..grid.scrollback_lines())
                .filter_map(|i| {
                    grid.get_history_line(i)
                        .map(|l| (l.to_string(), l.is_wrapped()))
                })
                .chain((0..grid.rows()).filter_map(|r| {
                    grid.storage
                        .row_at_screen(r)
                        .map(|row| (row.to_string(), row.is_wrapped()))
                }));
            let mut lines: Vec<String> = Vec::new();
            for (text, continues) in rows {
                match lines.last_mut() {
                    Some(last) if continues => last.push_str(&text),
                    _ => lines.push(text),
                }
            }
            let mut lines: Vec<String> = lines
                .into_iter()
                .map(|line| line.trim_end().to_string())
                .filter(|line| !line.is_empty())
                .collect();
            lines.sort();
            lines
        }
        /// Every character of `lines`, sorted: what a lost row would lack.
        fn text(lines: &[String]) -> String {
            let mut chars: Vec<char> = lines
                .iter()
                .flat_map(|line| line.chars())
                .filter(|c| !c.is_whitespace())
                .collect();
            chars.sort_unstable();
            chars.into_iter().collect()
        }
        /// A screen: its name, its lines, where the cursor ends (`None`: under
        /// the last line, as output leaves it), and whether its grid is
        /// tiered and its lines were drawn after the history with no clear,
        /// so the screen's top row continues a line whose head scrolled off.
        struct Shape {
            name: &'static str,
            lines: &'static [&'static str],
            cursor: Option<(u16, u16)>,
            tiered: bool,
            scrolled: bool,
        }
        const fn shape(
            name: &'static str,
            lines: &'static [&'static str],
            cursor: Option<(u16, u16)>,
            tiered: bool,
        ) -> Shape {
            Shape {
                name,
                lines,
                cursor,
                tiered,
                scrolled: false,
            }
        }
        const fn scrolled(
            name: &'static str,
            lines: &'static [&'static str],
            cursor: Option<(u16, u16)>,
            tiered: bool,
        ) -> Shape {
            Shape {
                scrolled: true,
                ..shape(name, lines, cursor, tiered)
            }
        }
        const SHORT: &[&str] = &["L0", "LLLLLLLLL1", "L2"];
        const HOMED: &[&str] = &["L0", "LLLLLLLLL1", "L2", "", "L4", "L5", "", "L7"];
        const MID: &[&str] = &["L0", "LLLLLLLLL1", "L2", "L3", "L4", "L5"];
        const ON_A_WRAP: &[&str] = &["L0", "LLLLLLLLLLLLLLLLLLLLLLLL1", "L2", "L3", "L4", "L5"];
        const LONG: &str = "LLLLLLLLLLLLLLLLLLLLLLLLL1";
        const SPLIT: &[&str] = &[LONG, "L2", "L3", "L4", "L5", "L6"];
        const SPLIT_ON_TAIL: &[&str] = &[LONG, "L2", "L3", "L4", "L5", "L6", ""];
        const SPLIT_PROMPT: &[&str] = &[LONG, "L2", "L3", "$ ", "", "", ""];
        const SHAPES: [Shape; 19] = [
            shape("a line that wraps below 10", SHORT, None, false),
            shape("the same, tiered", SHORT, None, true),
            shape(
                "a wrapped line",
                &["L0", "LLLLLLLLLLLLL1", "L2"],
                None,
                false,
            ),
            shape("long history", SHORT, None, false),
            shape(
                "a full screen",
                &["L0", "LLLLLLLLL1", "L2", "L3", "L4", "L5", "L6"],
                None,
                false,
            ),
            shape("a prompt", &["L0", "LLLLLLLLL1", "$ "], Some((2, 2)), false),
            shape("homed", HOMED, Some((0, 0)), false),
            shape("homed, tiered", HOMED, Some((0, 0)), true),
            shape(
                "homed over a blank tail",
                &["L0", "LLLLLLLLL1", "L2", "", "L4"],
                Some((0, 0)),
                false,
            ),
            shape("mid-screen", MID, Some((2, 1)), false),
            shape("mid-screen, tiered", MID, Some((2, 1)), true),
            shape(
                "on a wrapped line's second row",
                ON_A_WRAP,
                Some((2, 1)),
                false,
            ),
            shape("the same, tiered ", ON_A_WRAP, Some((2, 1)), true),
            scrolled("the screen's top inside a line", SPLIT, None, false),
            scrolled("the same, tiered  ", SPLIT, None, true),
            scrolled("the cursor on that top", SPLIT_ON_TAIL, Some((0, 1)), false),
            scrolled("the same, tiered   ", SPLIT_ON_TAIL, Some((0, 1)), true),
            scrolled("a prompt under that top", SPLIT_PROMPT, Some((4, 2)), false),
            scrolled("the same, tiered    ", SPLIT_PROMPT, Some((4, 2)), true),
        ];
        let start = |shape: &Shape| {
            let mut grid = if shape.tiered {
                let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
                Grid::with_tiered_scrollback(8, 10, 2, store)
            } else {
                Grid::with_scrollback(8, 10, 100)
            };
            let write = |grid: &mut Grid, line: &str| {
                for c in line.chars() {
                    grid.write_char_wrap(c);
                }
            };
            for i in 0..12 {
                let line = if shape.name == "long history" {
                    format!("hhhhhhhhh{i}xx")
                } else {
                    format!("h{i}")
                };
                write(&mut grid, &line);
                grid.carriage_return();
                grid.line_feed();
            }
            if !shape.scrolled {
                grid.erase_screen();
                grid.set_cursor(0, 0);
            }
            for (i, line) in shape.lines.iter().enumerate() {
                write(&mut grid, line);
                if i + 1 < shape.lines.len() || shape.cursor.is_none() {
                    grid.carriage_return();
                    grid.line_feed();
                }
            }
            if let Some((row, col)) = shape.cursor {
                grid.set_cursor(row, col);
            }
            if shape.scrolled {
                assert!(
                    grid.row(0).is_some_and(|row| row.is_wrapped()),
                    "{}: the screen's top row continues a line",
                    shape.name
                );
            }
            grid
        };
        let mut state: u64 = 0x94d0_49bb_1331_11eb;
        let runs: Vec<Vec<(u16, u16, bool)>> = (0..300)
            .map(|_| {
                let mut cols = 10;
                (0..5)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        let rows = 3 + u16::try_from((state >> 20) % 8).unwrap();
                        if (state >> 40).is_multiple_of(2) {
                            cols = [6, 7, 8, 9, 10, 11, 12, 14]
                                [usize::try_from((state >> 28) % 8).unwrap()];
                        }
                        (rows, cols, (state >> 50).is_multiple_of(3))
                    })
                    .collect()
            })
            .collect();
        // The runs that fail under `law`, and the first failure.
        let census = |law: ResizeLaw| {
            let mut failed = 0;
            let mut first = None;
            for shape in &SHAPES {
                // Lines are written only where the cursor ends the content.
                let writes = shape.cursor.is_none() || shape.name.starts_with("a prompt");
                for run in &runs {
                    let mut grid = start(shape);
                    grid.set_resize_law(law);
                    // The lines drawn, and every character they hold.
                    let mut lines = kept_lines(&grid);
                    let mut chars = text(&lines);
                    let mut written: Vec<String> = Vec::new();
                    for (i, &(rows, cols, write)) in run.iter().enumerate() {
                        grid.resize(rows, cols);
                        grid.assert_invariants();
                        let mut straight = start(shape);
                        straight.set_resize_law(law);
                        for line in &written {
                            write_line(&mut straight, line);
                        }
                        straight.resize(rows, cols);
                        let kept = kept_lines(&grid);
                        // Each kept line is a piece of one that was drawn, and
                        // together they hold every character drawn.
                        let whole = text(&kept) == chars
                            && kept.iter().all(|piece| {
                                lines.iter().any(|line| line.contains(piece.as_str()))
                            });
                        if !whole
                            || kept != kept_lines(&straight)
                            || picture(&grid) != picture(&straight)
                        {
                            failed += 1;
                            first.get_or_insert_with(|| {
                                format!(
                                    "{}: {run:?} at {i}: {:?} {kept:?}, straight there {:?} {:?}",
                                    shape.name,
                                    picture(&grid),
                                    picture(&straight),
                                    kept_lines(&straight)
                                )
                            });
                            break;
                        }
                        if write && writes {
                            let line = format!("o{i}");
                            let mut before = kept;
                            write_line(&mut grid, &line);
                            written.push(line);
                            let after = kept_lines(&grid);
                            chars = text(&after);
                            // The lines the write drew, a prompt's among them.
                            for line in after {
                                match before.iter().position(|old| *old == line) {
                                    Some(at) => drop(before.swap_remove(at)),
                                    None => lines.push(line),
                                }
                            }
                        }
                    }
                }
            }
            (failed, first)
        };
        let (failed, first) = census(ResizeLaw::Rewrap);
        assert_eq!(failed, 0, "{}", first.unwrap_or_default());
        let (failed, _) = census(ResizeLaw::PushedRows);
        assert!(
            failed > runs.len(),
            "only {failed} runs fail under the old law"
        );
    }

    /// A rows-only grow reveals the newest history wherever the grid keeps it:
    /// past a short ring, the lazy buffer's staged lines, then the tiered
    /// store's, as the ring grid fed the same writes reveals them. A grow that
    /// stopped at the ring appended blanks instead, so a grid whose history had
    /// just gone to its store (a rewrap, a restore from a checkpoint) grew
    /// differently from one whose had not, and a demoted row the ring could
    /// not hold never came back.
    #[test]
    fn a_rows_grow_reveals_the_history_a_tier_holds() {
        for lines in [2u16, 7] {
            for ring in [0usize, 2] {
                for promote in [false, true] {
                    for heights in [&[9u16][..], &[11], &[7, 8], &[5, 10, 8], &[3, 2, 9]] {
                        let at = format!("lines {lines}, ring {ring}, promoted {promote}");
                        let mut tiered = tiered_cleared_with_history(8, ring, 12, lines);
                        if promote {
                            tiered.drain_lazy_buffer();
                        }
                        let mut reference = cleared_with_history(8, 12, lines);
                        for &h in heights {
                            tiered.resize(h, 10);
                            reference.resize(h, 10);
                            assert_eq!(
                                picture(&tiered),
                                picture(&reference),
                                "{at}: {heights:?} at {h}"
                            );
                        }
                        tiered.assert_invariants();
                    }
                }
            }
        }
    }

    /// An erase of the whole screen from home (ED 0 with the cursor at the
    /// origin, which the resize journal counts as a full clear) forgets the
    /// revealed lines as ED 2 does: the program draws the new screen from the
    /// top, so the shrink back trims under it, matching the screen that never
    /// grew. An ED 0 issued below the top erases only part of the screen and
    /// keeps them.
    #[test]
    fn an_erase_from_home_forgets_the_revealed_rows() {
        let mut never = cleared_with_history(8, 12, 2);
        let mut flapped = cleared_with_history(8, 12, 2);
        flapped.resize(9, 10);
        for grid in [&mut never, &mut flapped] {
            grid.set_cursor(0, 0);
            grid.erase_to_end_of_screen();
            for i in 0..3 {
                write_line(grid, &format!("N{i}"));
            }
        }
        assert_eq!(flapped.revealed_rows(), 0, "a full clear from home");
        flapped.resize(8, 10);
        assert_eq!(screen(&flapped), screen(&never));
        assert_eq!(flapped.cursor_row(), never.cursor_row());
        flapped.assert_invariants();

        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(9, 10);
        grid.set_cursor(3, 0);
        grid.erase_to_end_of_screen();
        assert_eq!(grid.revealed_rows(), 1, "an ED 0 below the top");
    }

    /// A program that homes its cursor and draws over the revealed lines with
    /// no erase draws its screen from the top, as one that clears first does:
    /// the shrink back must trim under it, matching the screen that never grew.
    /// So must one that moves its cursor up among them by CUU or RI. The lines
    /// stayed owed, so the shrink demoted the program's own first line into
    /// history and every row ended one higher. A cursor that moves up only
    /// over the rows the program drew (a prompt's redraw) keeps them owed.
    #[test]
    fn a_redraw_from_home_forgets_the_revealed_rows() {
        /// A way home, named.
        type Home = (&'static str, fn(&mut Grid));
        let homes: [Home; 3] = [
            ("CUP", |grid| grid.set_cursor(0, 0)),
            ("CUU", |grid| grid.cursor_up(u16::MAX)),
            ("RI", |grid| {
                while grid.cursor_row() > 0 {
                    grid.reverse_line_feed();
                }
            }),
        ];
        for (how, home) in homes {
            let mut never = cleared_with_history(8, 12, 2);
            let mut flapped = cleared_with_history(8, 12, 2);
            flapped.resize(9, 10);
            for grid in [&mut never, &mut flapped] {
                home(grid);
                for i in 0..3 {
                    write_line(grid, &format!("N{i}p"));
                }
            }
            assert_eq!(flapped.revealed_rows(), 0, "{how}: drawn from the top");
            flapped.resize(8, 10);
            assert_eq!(screen(&flapped), screen(&never), "{how}");
            assert_eq!(flapped.cursor_row(), never.cursor_row(), "{how}");
            flapped.assert_invariants();
        }

        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(9, 10);
        grid.cursor_up(2);
        assert_eq!(grid.revealed_rows(), 1, "up over the drawn rows only");
    }
}
