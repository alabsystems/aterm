// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE SEAT RULE BESIDE THE BRAIN: what [`crate::pet_seat::decide`] reads, made from what a
//! pet's brain already knows, and the rule asked once a frame.
//!
//! The rule is asked in shadow: [`PetBrain`] keeps its answer ([`PetBrain::seat_shadow`], which
//! the trail film's status sidecar prints as `pet_seat=`) and draws nothing from it. The body on
//! glass is still seated by the brain's own laws. Until something draws from it the rule is
//! asked only in a build with debug assertions (a test, the film, a lab instance), so a shipped
//! build pays nothing for an answer nobody sees.
//!
//! A pet's art reaches the rule through [`SeatArt`] alone: three numbers, from which the rule
//! gets a box of whole cells. The cat and the dog are its first two clients; a pet added as a
//! file gives the same three numbers and gets every seat without a line of the rule changing.
//!
//! What each input is made from:
//!
//! | input | made from |
//! | --- | --- |
//! | the page | the pet's own reading of the pane, the console's world ([`PetBrain::observe_console_with_reserve`], [`PetWorld::seat_cell`]), which the host takes from the frame's cells before the pet's tick whatever other effect is on: a glyph, a coloured ground, the host's chrome, an image and a selection are lettered, and so is a cell the world did not read. A row drawn as a rule is a fence ([`PetWorld::rule_line`]) |
//! | no page | a frame whose world is not read, or is of another grid than the frame's, asks nothing: the seat stands as it was on the last page read, the shadow says `unread`, and the next page read is met as another page (no letter landed on a seat nobody was reading). A host that never reads a page for the pet has no pet seated by this rule |
//! | the caret | the frame's caret; one past the last column or row (a wrap pending) is read at the last ([`SeatShadow::caret_clamped`]) |
//! | the body | [`SeatArt::seat_body`] of the species |
//! | `ahead` | three keys past the caret while its own forward steps are fresh, each as wide as the latest step (a wide glyph is two cells); the rest of the row while a program streams on it |
//! | `below_is_next` | a shell at its prompt: no command running and not a full-screen page |
//! | `quiet_ms` | since the lettered cells last changed on the rows [`Rule::reads`] names, the caret moved, the page scrolled or became another |
//! | `hold` | on the glass and in the air, with a paste waiting for its Return, or asleep |
//! | `new_page` | the page read is of another terminal, screen or grid than the last one read ([`PetWorldStamp`]), or the view was scrolled; the brain was told of another screen or a re-grid ([`PetBrain::rescreen`], [`PetBrain::rebase_geometry`]); another surface; a page read after frames that read none |
//! | `scrolled`, the ride | the page's own count of the rows it moved ([`PetWorldStamp`]). A seat is beside the caret: where the caret stayed on its row (a prompt on a full page's last row, a build printing there) the seat stays too, and the rule is told the page scrolled under it. Only where the caret went up with the page does the seat ride with them |
//!
//! The page is read for this frame before the pet's tick, and what tells the rule of another
//! page is the page's own stamp, so the rule never meets a new screen or a new grid with the
//! old one's letters, nor a frame late.
//!
//! A letter that changes to another letter without the cell going blank (a spinner in a
//! sentence) does not reset the quiet; the host's content sequence is page-wide and would count
//! a clock in a far corner, which the rule says not to.

use std::fmt;

use aterm_time::{Duration, Instant};

use super::{ART_ROWS, PetAction, PetBrain, PetSense, PetSpecies, art_cols, head_law};
use crate::pet_seat::{Body, Edge, Page, Path, Rule, Scene, Seat, SeatState, Why, decide, path};
use crate::pet_world::{PetWorld, PetWorldStamp};

/// What a pet's art gives the seat rule. Nothing else of a pet reaches it: no species, no pose,
/// no facing.
pub trait SeatArt {
    /// How many cells across its art is drawn on cells `cell_w` by `cell_h` px.
    fn seat_art_cols(&self, cell_w: u16, cell_h: u16) -> f32;

    /// How many rows tall its art is drawn.
    fn seat_art_rows(&self) -> f32;

    /// The share of its size it may be drawn at where the whole drawing finds no room (`0.0`:
    /// it has no small drawing).
    fn seat_small_fit(&self) -> f32;

    /// The body the rule seats: the whole cells each drawing touches.
    #[inline]
    fn seat_body(&self, cell_w: u16, cell_h: u16) -> Body {
        Body::of_art(
            self.seat_art_cols(cell_w, cell_h),
            self.seat_art_rows(),
            self.seat_small_fit(),
        )
    }
}

impl SeatArt for PetSpecies {
    #[inline]
    fn seat_art_cols(&self, cell_w: u16, cell_h: u16) -> f32 {
        // The cat's and the dog's art share one box ([`art_cols`], [`ART_ROWS`]).
        art_cols(cell_w, cell_h)
    }

    #[inline]
    fn seat_art_rows(&self) -> f32 {
        ART_ROWS
    }

    #[inline]
    fn seat_small_fit(&self) -> f32 {
        // The share a body between two lettered rows is fitted to today.
        head_law(true, true).1
    }
}

/// The pet's own reading of the pane as the rule's page.
#[derive(Clone, Copy, Debug)]
pub(super) struct WorldPage<'a> {
    rows: u16,
    cols: u16,
    world: &'a PetWorld,
}

impl<'a> WorldPage<'a> {
    /// The page of a frame of `rows` by `cols` cells: `None` where `world` read no frame, or
    /// one of another grid.
    #[inline]
    pub(super) fn read(rows: u16, cols: u16, world: &'a PetWorld) -> Option<(Self, PetWorldStamp)> {
        let stamp = world.stamp()?;
        (stamp.surface.rows == usize::from(rows) && stamp.surface.cols == usize::from(cols))
            .then_some((Self { rows, cols, world }, stamp))
    }

    /// The lettered cells of the rows `first..=last`, folded to one word: it changes when a
    /// cell of them goes from blank to lettered or back.
    #[inline]
    fn rows_sig(&self, first: u16, last: u16) -> u64 {
        let mut sig = 0xcbf2_9ce4_8422_2325_u64 ^ (u64::from(first) << 32 | u64::from(last));
        for row in first..=last.min(self.rows.saturating_sub(1)) {
            let mut word = 0u64;
            for col in 0..self.cols {
                word = word << 1 | u64::from(self.lettered(row, col));
                if col % 64 == 63 {
                    sig = (sig ^ word).wrapping_mul(0x0000_0100_0000_01b3);
                    word = 0;
                }
            }
            sig = (sig ^ word ^ u64::from(row) << 48).wrapping_mul(0x0000_0100_0000_01b3);
        }
        sig
    }
}

impl Page for WorldPage<'_> {
    #[inline]
    fn rows(&self) -> u16 {
        self.rows
    }

    #[inline]
    fn cols(&self) -> u16 {
        self.cols
    }

    #[inline]
    fn lettered(&self, row: u16, col: u16) -> bool {
        // A cell the world did not read is no seat's: nobody knows what is on it.
        self.world.seat_cell(usize::from(row), usize::from(col)) != Some(false)
    }

    #[inline]
    fn fence(&self, row: u16) -> bool {
        matches!(self.world.rule_line(usize::from(row)), Some(true))
    }
}

/// Whether two stamps are of one page: one terminal's one screen on one grid, seen at one place
/// in its history.
#[inline]
const fn one_page(a: PetWorldStamp, b: PetWorldStamp) -> bool {
    let (s, t) = (a.surface, b.surface);
    s.session == t.session
        && s.terminal_id == t.terminal_id
        && s.alt_screen == t.alt_screen
        && s.rows == t.rows
        && s.cols == t.cols
        && a.display_offset == b.display_offset
}

/// What one page's rows did between two of its stamps: whether any moved, and by how many rows
/// the whole page rose where that is all that moved.
#[inline]
const fn rows_moved(a: PetWorldStamp, b: PetWorldStamp) -> (bool, u16) {
    let (s, t) = (a.surface, b.surface);
    let other = s.absolute_row_revision != t.absolute_row_revision
        || s.history_renumber_epoch != t.history_renumber_epoch
        || s.scroll_invalidation != t.scroll_invalidation
        || a.scroll_band_batches != b.scroll_band_batches
        || a.scroll_band_seq != b.scroll_band_seq;
    let up = b.uniform_up_rows.wrapping_sub(a.uniform_up_rows);
    let moved = other || up != 0 || a.top_absolute_row != b.top_absolute_row;
    let rose = if !other && up > 0 && up <= u16::MAX as u64 {
        up as u16
    } else {
        0
    };
    (moved, rose)
}

/// How long (ms) after the caret's last forward step the hand is still writing.
const HAND_FRESH_MS: u32 = 300;

/// How many keys ahead of the caret a writing hand is given. It is under the rule's free lead
/// ([`Rule::lead`]) on purpose: a pet resting at the lead's far end is then left alone by a
/// key, and the writing sends it on along its row a seat or two at a step. Read from the
/// hand's pace instead (the keys it strikes in a dwell: 5 at 20 keys a second, more for a
/// faster one), the cells ahead outran the lead and the row over the line became the cheaper
/// seat: the pet leapt off the typed row at the second key of every fast run.
const AHEAD_KEYS: u16 = 3;

/// The longest span (ms) one asking counts: a host that slept longer is met as one that slept
/// this long, as the brain's own clocks are.
const SPAN_MAX_MS: u128 = 5_000;

/// The seat rule's answer at the last asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatShadow {
    /// The seat.
    pub seat: Seat,
    /// Why the rule answered so.
    pub why: Why,
    /// The way from the seat before to this one.
    pub path: Path,
    /// The cells past the caret the rule was told the writing takes.
    pub ahead: u16,
    /// The quiet (ms) of the rows about the caret the rule was told.
    pub quiet_ms: u32,
    /// Whether the pet was held to a seat that is still one.
    pub hold: bool,
    /// Whether the frame's caret was past the pane's last column or row and was read at the
    /// last.
    pub caret_clamped: bool,
    /// Whether the frame had no page to read, so the rule was not asked on it and the seat is
    /// the one it had on the last page read.
    pub unread: bool,
    /// Whether the rule has been asked at all.
    pub asked: bool,
}

impl SeatShadow {
    const NONE: Self = Self {
        seat: Seat::Off(Edge::Right),
        why: Why::Kept,
        path: Path::Stay,
        ahead: 0,
        quiet_ms: 0,
        hold: false,
        caret_clamped: false,
        unread: false,
        asked: false,
    };
}

/// One word with no space in it, for a `key=value` status line: `none` before the first
/// asking, else `<seat>/<why>/<path>/a<ahead>/q<quiet ms>` with `/held`, `/clamped` and
/// `/unread` where they hold. A seat is `r<row>c<col>w` (whole) or `r<row>c<col>s` (small), or
/// `off-left`, `off-right`.
impl fmt::Display for SeatShadow {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.asked {
            return f.write_str("none");
        }
        match self.seat {
            Seat::On { row, col, small } => {
                write!(f, "r{row}c{col}{}", if small { 's' } else { 'w' })?;
            }
            Seat::Off(Edge::Left) => f.write_str("off-left")?,
            Seat::Off(Edge::Right) => f.write_str("off-right")?,
        }
        let why = match self.why {
            Why::Kept => "kept",
            Why::Yielded => "yielded",
            Why::GaveWay => "gave-way",
            Why::Switched => "switched",
            Why::Moved => "moved",
            Why::Returned => "returned",
        };
        let way = match self.path {
            Path::Stay => "stay",
            Path::Walk => "walk",
            Path::Leap => "leap",
            Path::Exit(Edge::Left) => "exit-left",
            Path::Exit(Edge::Right) => "exit-right",
            Path::Enter(Edge::Left) => "enter-left",
            Path::Enter(Edge::Right) => "enter-right",
            Path::Cut => "cut",
        };
        write!(f, "/{why}/{way}/a{}/q{}", self.ahead, self.quiet_ms)?;
        if self.hold {
            f.write_str("/held")?;
        }
        if self.caret_clamped {
            f.write_str("/clamped")?;
        }
        if self.unread {
            f.write_str("/unread")?;
        }
        Ok(())
    }
}

/// What the brain carries for the seat rule from one asking to the next.
#[derive(Clone, Copy, Debug)]
pub(super) struct SeatWire {
    state: SeatState,
    shadow: SeatShadow,
    asked_at: Option<Instant>,
    /// The caret the rule was last asked about.
    caret: Option<(u16, u16)>,
    /// The stamp of the page the rule last read, the rows it read about the caret then
    /// (`first, last`, as [`Rule::reads`]) and their lettered cells ([`WorldPage::rows_sig`]).
    stamp: Option<PetWorldStamp>,
    reads: (u16, u16),
    rows_sig: u64,
    /// The grid and the surface the rule was last asked on.
    grid: (u16, u16),
    surface: Option<(u64, bool)>,
    quiet_ms: u32,
    /// How long (ms) since the caret's last forward step (`u32::MAX`: no run is open), and
    /// how many cells that step took.
    since_step_ms: u32,
    step_cells: u16,
    /// The next page read is another page: the brain was told so, or a frame read none.
    new_page: bool,
    /// How long (ms) after the last asking the rule's answer may change with no news from the
    /// page ([`PetBrain::seat_due`]); `None`: not until a frame brings some.
    due_ms: Option<u32>,
}

impl SeatWire {
    pub(super) const fn new() -> Self {
        Self {
            state: SeatState::offstage(Edge::Right),
            shadow: SeatShadow::NONE,
            asked_at: None,
            caret: None,
            stamp: None,
            reads: (0, 0),
            rows_sig: 0,
            grid: (0, 0),
            surface: None,
            quiet_ms: 0,
            since_step_ms: u32::MAX,
            step_cells: 1,
            new_page: false,
            due_ms: None,
        }
    }

    /// The page is another page: the other screen, a re-gridded page. The rule is told at the
    /// next page it reads, which is the first one of the new screen or grid.
    #[inline]
    pub(super) const fn note_new_page(&mut self) {
        self.new_page = true;
    }

    /// The cells a caret that went from `was` to `is` stepped forward by as a writing hand
    /// steps it: one or two along its row, or on over the wrap to the next row's first cells
    /// (the same row where the last row's wrap scrolled the page).
    #[inline]
    const fn hand_step(was: (u16, u16), is: (u16, u16), cols: u16) -> Option<u16> {
        if is.0 == was.0 && is.1 > was.1 && is.1 - was.1 <= 2 {
            return Some(is.1 - was.1);
        }
        let wrapped = is.1 <= 1
            && was.1.saturating_add(2) >= cols
            && (is.0 == was.0.saturating_add(1) || is.0 == was.0);
        if wrapped && is.1 != was.1 {
            return Some(1);
        }
        None
    }

    /// The cells past the caret a writing hand takes next: [`AHEAD_KEYS`] keys, each the width
    /// of its latest step, no more than the rule's reach; none once its last step is no longer
    /// fresh ([`HAND_FRESH_MS`]).
    #[inline]
    const fn hand_ahead(&self, rule: &Rule) -> u16 {
        if self.since_step_ms > HAND_FRESH_MS {
            return 0;
        }
        let cells = AHEAD_KEYS.saturating_mul(self.step_cells);
        if cells < rule.reach {
            cells
        } else {
            rule.reach
        }
    }

    /// A frame the rule is not asked on: no clock of the rule's runs to a deadline on it.
    #[inline]
    const fn unasked(&mut self) {
        self.due_ms = None;
    }
}

impl PetBrain {
    /// The seat rule's answer at the last asking ([`SeatShadow`]). Nothing is drawn from it.
    #[must_use]
    #[inline]
    pub const fn seat_shadow(&self) -> SeatShadow {
        self.seat.shadow
    }

    /// WHEN THE SEAT RULE'S ANSWER MAY NEXT CHANGE with no news from the page: a better seat's
    /// dwell running out, the quiet a pet off the glass owes, a seat it left being one again,
    /// the hand's last step going stale. The rule reads no clock and is asked once a frame, so
    /// a host that draws on demand has to be woken for these: unwoken, a pet that walked off the
    /// glass comes back only when something else draws. `None`: not until a frame brings news,
    /// or while the scene holds the pet (the hold's end is the brain's own frame). While the
    /// rule draws nothing no host is woken by it ([`Self::next_change_deadline`]): what draws
    /// from the rule folds this deadline into that offer.
    #[must_use]
    #[inline]
    pub fn seat_due(&self) -> Option<Instant> {
        let (at, ms) = (self.seat.asked_at?, self.seat.due_ms?);
        Some(at + Duration::from_millis(u64::from(ms)))
    }

    /// ASK THE SEAT RULE, once a host frame, after the brain lived it. A frame with no caret
    /// in the pet's space asks nothing: there is nothing to sit beside, and the next asking
    /// counts the span since this one. A frame with no page read asks nothing either: the seat
    /// stands, and the next page read is another page.
    #[inline]
    pub(super) fn ask_the_seat_rule(&mut self, sense: PetSense) {
        let mut wire = self.seat;
        let Some((row, col)) = sense.caret else {
            wire.unasked();
            self.seat = wire;
            return;
        };
        if sense.rows == 0 || sense.cols == 0 {
            wire.unasked();
            self.seat = wire;
            return;
        }
        let read = self
            .console
            .world
            .as_deref()
            .and_then(|world| WorldPage::read(sense.rows, sense.cols, world));
        let Some((page, stamp)) = read else {
            wire.unasked();
            wire.new_page = true;
            if wire.shadow.asked {
                wire.shadow = SeatShadow {
                    why: Why::Kept,
                    path: Path::Stay,
                    unread: true,
                    ..wire.shadow
                };
            }
            self.seat = wire;
            return;
        };
        let caret = (row.min(sense.rows - 1), col.min(sense.cols - 1));
        let caret_clamped = caret != (row, col);
        let dt_ms = match wire.asked_at {
            Some(prev) => sense
                .now
                .saturating_duration_since(prev)
                .as_millis()
                .min(SPAN_MAX_MS) as u32,
            None => 0,
        };
        let first = wire.asked_at.is_none();
        let grid = (sense.rows, sense.cols);
        let new_page = core::mem::take(&mut wire.new_page)
            || (!first && (wire.grid != grid || wire.surface != self.surface))
            || matches!(wire.stamp, Some(prev) if !one_page(prev, stamp));
        let (rows_moved, rose) = match wire.stamp {
            Some(prev) if !new_page => rows_moved(prev, stamp),
            _ => (false, 0),
        };
        let was = wire.caret;
        // THE SEAT IS BESIDE THE CARET, NOT ON THE PAGE. It rides a scroll only where the caret
        // went up with the page. Carried up from beside a caret that stayed on its row (a
        // build printing on a full page's last row), it sat one row over the line each time a
        // line was printed, the line was printed under it, and it leapt back down to the seat
        // it had: a leap in place for every line of output.
        let rode = rose > 0 && matches!(was, Some((r, _)) if r >= rose && caret.0 == r - rose);
        if rode {
            wire.state = wire.state.ride(i32::from(rose), sense.rows, sense.cols);
        }
        let scrolled = rows_moved && !rode;
        let moved = was != Some(caret);
        let rule = if self.full_screen {
            Rule::FULL_SCREEN
        } else {
            Rule::DEFAULT
        };
        let body = self.species.seat_body(sense.cell_w, sense.cell_h);
        let reads = rule.reads(body, caret.0, sense.rows);
        // The rows are read again only where the page was: a frame of the same page costs the
        // quiet nothing.
        let rows_sig = if wire.stamp == Some(stamp) && wire.reads == reads {
            wire.rows_sig
        } else {
            page.rows_sig(reads.0, reads.1)
        };
        if first || rows_sig != wire.rows_sig || moved || rows_moved || new_page {
            wire.quiet_ms = 0;
        } else {
            wire.quiet_ms = wire.quiet_ms.saturating_add(dt_ms);
        }
        match was {
            Some(was) if !new_page => match SeatWire::hand_step(was, caret, sense.cols) {
                Some(cells) => {
                    wire.since_step_ms = 0;
                    wire.step_cells = cells;
                }
                // A caret that went anywhere else ended the run.
                None if moved => wire.since_step_ms = u32::MAX,
                None => wire.since_step_ms = wire.since_step_ms.saturating_add(dt_ms),
            },
            _ => wire.since_step_ms = u32::MAX,
        }
        let ahead = if self.stream {
            // A program writing in chunks takes its row to the end.
            (sense.cols - 1 - caret.1).max(wire.hand_ahead(&rule))
        } else {
            wire.hand_ahead(&rule)
        };
        // A hold is a body's on the glass: off it there is no pose to keep, and a held brain
        // must not keep the pet from coming on.
        let hold = wire.state.seat.on_page()
            && (self.flight.is_some()
                || self.paste_holding()
                || matches!(self.action, PetAction::Sleep));
        let scene = Scene {
            page: &page,
            caret,
            body,
            ahead,
            below_is_next: self.exec_since.is_none() && !self.full_screen,
            quiet_ms: wire.quiet_ms,
            hold,
            new_page,
            scrolled,
        };
        let decision = decide(&rule, &scene, wire.state, dt_ms);
        let way = path(&rule, &scene, wire.state.seat, decision.state.seat);
        let state = decision.state;
        let mut due: Option<u32> = None;
        let mut by = |ms: u32| due = Some(due.map_or(ms, |d| d.min(ms)));
        if decision.better && !hold {
            let owed = if !state.seat.on_page() && state.quiet_owed {
                rule.quiet_owed_ms(state.strikes)
                    .saturating_sub(wire.quiet_ms)
            } else {
                0
            };
            by(rule.dwell_ms.saturating_sub(state.beaten_ms).max(owed));
        }
        if decision.why != Why::Kept {
            // A seat just taken is judged against the others a dwell on: the walk from where a
            // pet came in to the end of the free lead is asked for by no news.
            by(rule.dwell_ms);
        }
        for bar in state.barred.iter().flatten() {
            by(rule.dwell_ms.saturating_sub(bar.ago_ms));
        }
        if wire.since_step_ms <= HAND_FRESH_MS {
            by(HAND_FRESH_MS - wire.since_step_ms + 1);
        }
        wire.due_ms = due;
        wire.state = state;
        wire.shadow = SeatShadow {
            seat: state.seat,
            why: decision.why,
            path: way,
            ahead,
            quiet_ms: wire.quiet_ms,
            hold,
            caret_clamped,
            unread: false,
            asked: true,
        };
        wire.asked_at = Some(sense.now);
        wire.caret = Some(caret);
        wire.stamp = Some(stamp);
        wire.reads = reads;
        wire.rows_sig = rows_sig;
        wire.grid = grid;
        wire.surface = self.surface;
        self.seat = wire;
    }
}

#[cfg(test)]
#[path = "seat_tests.rs"]
mod tests;
