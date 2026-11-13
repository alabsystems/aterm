// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE SEAT RULE, ONE PATH: where a pet sits beside a caret, and how it gets there.
//!
//! A seat is a box of whole cells that covers no lettered cell, not the caret's cell and none
//! of the cells the writing is about to take; or it is off the glass, through the pane's left or
//! right edge, which is always a seat. Every seat has a cost ([`Rule`]), the seat with the least
//! cost is the best one ([`best`]), and a pet changes seat only for a reason that lasts
//! ([`decide`]): at once when its seat stops being one (off the glass where a letter landed on
//! it, to the best seat where the caret or the writing came into it, the page scrolled its rows
//! under it or became another page), and otherwise once another seat has been better by
//! [`Rule::margin`] for [`Rule::dwell_ms`]. Along the caret's row a seat is beside the line where
//! it is past the line's end ([`line_past`]: the caret, or the end of a suggestion that runs on
//! from it), and no seat is sought across a rule a program draws over or under its input line
//! ([`Page::fence`]). A pet that left the glass comes back only to a seat it can walk to from
//! an edge and the writing cannot reach within a dwell ([`Rule::reach`]), and only once the rows
//! about the caret have been quiet for [`Rule::return_quiet_ms`], longer after each letter that
//! took it off ([`SeatState::strikes`]). The way between two seats is one of five ([`path`]).
//!
//! The rule knows a body's size in cells and nothing else about it: no species, no pose, no
//! face. It reads no clock (the caller says how long ago things were) and no global, allocates
//! nothing, and uses whole numbers only, so the same inputs give the same seat on every host.
//!
//! Nothing draws from this yet: in a build with debug assertions `kitty_pet`'s `PetBrain` asks
//! it once a frame and keeps the answer beside the seat it chooses itself
//! (`PetBrain::seat_shadow`), which is the one on glass.

/// A page as the seat rule reads it: a grid of cells, each lettered or not. A lettered cell is
/// any cell a body must not cover: a glyph, a wide glyph's second cell, a cell a neighbour's
/// glyph overhangs, a word icon, a rule or other chrome.
pub trait Page {
    /// The pane's rows.
    fn rows(&self) -> u16;
    /// The pane's columns.
    fn cols(&self) -> u16;
    /// Whether the cell is lettered. Asked only for a cell inside the pane.
    fn lettered(&self, row: u16, col: u16) -> bool;
    /// Whether the row is a fence: a rule a program draws across the pane (the lines over and
    /// under an input box). No seat is sought across one from the caret: the rows past it are
    /// another part of the program's page (its footer, its transcript), not the line's. A page
    /// that knows of none has none.
    #[inline]
    fn fence(&self, _row: u16) -> bool {
        false
    }
}

/// A page held as bits the caller owns: row-major, [`BitPage::stride`] words a row, a cell's bit
/// set where it is lettered.
#[derive(Clone, Copy, Debug)]
pub struct BitPage<'a> {
    rows: u16,
    cols: u16,
    words: &'a [u64],
}

impl<'a> BitPage<'a> {
    /// The words one row of `cols` cells takes.
    #[must_use]
    #[inline]
    pub const fn stride(cols: u16) -> usize {
        (cols as usize).div_ceil(64)
    }

    /// A page of `rows` by `cols` cells over `words`; `None` where `words` is too short for it.
    #[must_use]
    #[inline]
    pub const fn new(rows: u16, cols: u16, words: &'a [u64]) -> Option<Self> {
        if words.len() < rows as usize * Self::stride(cols) {
            return None;
        }
        Some(Self { rows, cols, words })
    }
}

impl Page for BitPage<'_> {
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
        let at = usize::from(row) * Self::stride(self.cols) + usize::from(col) / 64;
        self.words.get(at).is_some_and(|w| w >> (col % 64) & 1 == 1)
    }
}

/// A box of whole cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    /// Its width in cells.
    pub cols: u16,
    /// Its height in cells, up from the row its feet stand on.
    pub rows: u16,
}

/// A body as the seat rule reads it: the cells its box takes drawn whole, and the cells of a
/// smaller drawing it may take where the whole one finds no room (`None`: it has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Body {
    /// The body drawn whole.
    pub whole: Size,
    /// The body drawn small.
    pub small: Option<Size>,
}

impl Body {
    /// The body of art `width` cells across and `height` rows tall, with a smaller drawing at
    /// `fit` of its size (`0.0`: none): each box is the whole cells the art touches.
    #[must_use]
    #[inline]
    pub const fn of_art(width: f32, height: f32, fit: f32) -> Self {
        let whole = Size {
            cols: cells(width),
            rows: cells(height),
        };
        let small = if fit > 0.0 && fit < 1.0 {
            Some(Size {
                cols: cells(width * fit),
                rows: cells(height * fit),
            })
        } else {
            None
        };
        Self { whole, small }
    }

    #[inline]
    const fn size(&self, small: bool) -> Option<Size> {
        if small { self.small } else { Some(self.whole) }
    }
}

/// The whole cells a length covers, one at least.
#[inline]
const fn cells(len: f32) -> u16 {
    if len.is_nan() || len <= 1.0 {
        return 1;
    }
    if len >= 65_535.0 {
        return u16::MAX;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "between 1 and 65535, checked above"
    )]
    let whole = len as u16;
    if (whole as f32) < len {
        whole + 1
    } else {
        whole
    }
}

/// A side edge of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The pane's left edge.
    Left,
    /// The pane's right edge.
    Right,
}

/// Where a pet sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    /// On the page: its feet on `row`, its box's left edge at `col`, drawn small or whole.
    On {
        /// The row its feet stand on.
        row: u16,
        /// Its box's first column.
        col: u16,
        /// Drawn small ([`Body::small`]).
        small: bool,
    },
    /// Off the glass, through the edge it left by.
    Off(Edge),
}

impl Seat {
    /// Whether the two are one seat. Off the glass is one seat, whichever edge it left by.
    #[must_use]
    #[inline]
    pub const fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::Off(_), Self::Off(_)) => true,
            (
                Self::On { row, col, small },
                Self::On {
                    row: r,
                    col: c,
                    small: s,
                },
            ) => row == r && col == c && small == s,
            _ => false,
        }
    }

    /// Whether the seat is on the page.
    #[must_use]
    #[inline]
    pub const fn on_page(self) -> bool {
        matches!(self, Self::On { .. })
    }
}

/// The scoring rule's numbers. A cost is in cells: one cell of distance along a row costs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// How many rows over the caret's a seat's feet may stand.
    pub rows_up: u16,
    /// How many rows under the caret's a seat's feet may stand (`0` on a full-screen program's
    /// page, whose rows under its line are its own).
    pub rows_down: u16,
    /// The fewest cells past the caret the writing is read to take ([`Scene::ahead`]): the
    /// caret's next cell is no seat, so a first key moves the caret beside the pet, not under
    /// it.
    pub gap: u16,
    /// How far ahead of the caret on its row a seat costs nothing: the room a pet keeps ahead
    /// of a writing hand, so a seat the writing pushed ahead is not left when the hand pauses.
    pub lead: u16,
    /// What each of the first two rows between the feet and the caret's row costs.
    pub row: u32,
    /// What each row past the second costs. With two rows it is more than [`Rule::small`] on
    /// purpose: the small drawing on the row a person types on comes before the whole one
    /// three rows away. With every row at one price the whole pet three rows under a line
    /// between two lettered rows cost 6, or 8 at a prompt, against the small one's 9 beside the
    /// caret, and the pet left the typed row.
    pub far_row: u32,
    /// What a seat under the caret's row costs more while the next line prints there
    /// ([`Scene::below_is_next`]).
    pub below: u32,
    /// What a seat behind the caret costs more than one ahead of it.
    pub behind: u32,
    /// What the small drawing costs more than the whole one.
    pub small: u32,
    /// What a seat off the glass costs, more than the cells to the nearer edge.
    pub off: u32,
    /// By how much another seat must be better before the pet leaves a seat that is still one.
    pub margin: u32,
    /// How long (ms) it must stay better by that much, and how long a seat just left stays
    /// barred.
    pub dwell_ms: u32,
    /// How many cells past the caret a hand may take within one dwell: a pet comes back on the
    /// glass only to a seat clear of them, so the first keys after its return do not send it
    /// off again.
    pub reach: u16,
    /// How long (ms) the rows about the caret must have been quiet ([`Scene::quiet_ms`]) before
    /// a pet that left the glass comes back ([`SeatState::quiet_owed`]).
    pub return_quiet_ms: u32,
    /// How many times that quiet doubles at most: it doubles with each letter that took the pet
    /// off the glass after the first ([`SeatState::strikes`]).
    pub strikes_cap: u8,
    /// How long (ms) on the glass with no letter landing under it forgives those letters.
    pub forgive_ms: u32,
    /// How many blank cells between two letters past the caret on its row still join them
    /// into one line ([`line_past`]): a space between a suggestion's words, not the gap before a
    /// right-hand prompt.
    pub line_join: u16,
}

impl Rule {
    /// The rule for a shell's page.
    pub const DEFAULT: Self = Self {
        rows_up: 1,
        rows_down: 3,
        gap: 1,
        lead: 4,
        row: 2,
        far_row: 10,
        below: 2,
        behind: 1,
        small: 9,
        off: 24,
        margin: 2,
        dwell_ms: 250,
        reach: 8,
        return_quiet_ms: 1_500,
        strikes_cap: 3,
        forgive_ms: 30_000,
        line_join: 2,
    };

    /// The rule for a full-screen program's page: no seat under the caret's row.
    pub const FULL_SCREEN: Self = Self {
        rows_down: 0,
        ..Self::DEFAULT
    };
}

impl Rule {
    /// The first and the last row of a pane of `rows` rows the rule reads for a caret on
    /// `caret_row`: the rows a seat's feet may stand on and the rows over them `body`'s box
    /// reaches. [`Scene::quiet_ms`] is the quiet of these rows.
    #[must_use]
    #[inline]
    pub const fn reads(&self, body: Body, caret_row: u16, rows: u16) -> (u16, u16) {
        let first = caret_row
            .saturating_sub(self.rows_up)
            .saturating_sub(body.whole.rows.saturating_sub(1));
        let last = caret_row.saturating_add(self.rows_down);
        let end = rows.saturating_sub(1);
        (
            if first < end { first } else { end },
            if last < end { last } else { end },
        )
    }

    /// The quiet (ms) a pet that left the glass owes before it comes back, after `strikes`
    /// letters took it off: [`Rule::return_quiet_ms`], doubled for each strike after the first
    /// up to [`Rule::strikes_cap`] times.
    #[must_use]
    #[inline]
    pub const fn quiet_owed_ms(&self, strikes: u8) -> u32 {
        let doubled = strikes.saturating_sub(1);
        let doubled = if doubled < self.strikes_cap {
            doubled
        } else {
            self.strikes_cap
        };
        let times = if doubled < 31 {
            1u32 << doubled
        } else {
            u32::MAX
        };
        self.return_quiet_ms.saturating_mul(times)
    }
}

impl Default for Rule {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What the seat rule reads each time it is asked.
#[derive(Debug)]
pub struct Scene<'a, P: Page> {
    /// The page.
    pub page: &'a P,
    /// The caret's cell, `(row, col)`.
    pub caret: (u16, u16),
    /// The body to seat.
    pub body: Body,
    /// How many cells past the caret the writing will take before the pet could be out of
    /// their way, at the hand's tempo (`0` at rest): a wide glyph is two, and a program's
    /// stream may take the rest of the row. They run along the caret's row and on from the
    /// next row's first cell where the row ends.
    pub ahead: u16,
    /// Whether the next line prints under the caret's row (a shell at its prompt).
    pub below_is_next: bool,
    /// How long (ms) since a cell changed on the rows the rule reads about the caret
    /// ([`Rule::reads`]) or the caret moved. A change elsewhere on the page (a spinner, a clock)
    /// is not counted: no seat and no way in reads it.
    pub quiet_ms: u32,
    /// Whether the pet may not leave a seat that is still one (mid-word, in the air, asleep).
    pub hold: bool,
    /// Whether the page is another page than at the last asking: the other screen, a cleared or
    /// reflowed page. Letters under the seat then landed on nothing: the page was exchanged.
    pub new_page: bool,
    /// Whether the page's rows moved under the pet since the last asking and its seat did not
    /// move with them (a scroll under a caret that stayed on its row). Letters under the seat
    /// then came up with the page; none landed on it.
    pub scrolled: bool,
}

impl<P: Page> Clone for Scene<'_, P> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Page> Copy for Scene<'_, P> {}

/// How many runs of seats a pet can have left within one dwell: chased out of one more, it
/// leaves the glass.
pub const BARS: usize = 4;

/// A run of seats along one row the pet left a short while ago: the seat it gave up, and where
/// it walked on along its row, the seats it passed. None of them is a seat again until the
/// dwell has passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    /// The row the seats' feet stand on.
    pub row: u16,
    /// Whether they are seats of the small drawing.
    pub small: bool,
    /// The first of their first columns.
    pub first: u16,
    /// The last of their first columns.
    pub last: u16,
    /// How long (ms) ago the pet left the last of them.
    pub ago_ms: u32,
}

impl Bar {
    #[inline]
    const fn holds(&self, seat: Seat) -> bool {
        match seat {
            Seat::On { row, col, small } => {
                row == self.row && small == self.small && col >= self.first && col <= self.last
            }
            Seat::Off(_) => false,
        }
    }
}

/// What a pet's seat carries from one asking to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatState {
    /// Where it sits.
    pub seat: Seat,
    /// The caret's cell at the last asking (`None` before the first, and where a scroll
    /// carried it off the pane): letters between it and the caret now are the hand's writing.
    pub caret: Option<(u16, u16)>,
    /// The seats it left within the dwell.
    pub barred: [Option<Bar>; BARS],
    /// How long (ms) another seat has been better by the margin without a break.
    pub beaten_ms: u32,
    /// Whether it left the glass (for the writing, for a letter under it, carried off by the
    /// page): it then comes back only once the rows about the caret have been quiet
    /// ([`Rule::quiet_owed_ms`]). A pet that has not come on yet owes no quiet.
    pub quiet_owed: bool,
    /// How many letters took it off the glass ([`Why::Yielded`]) and are not forgiven yet: each
    /// after the first doubles the quiet it owes, so a letter that comes and goes under a seat
    /// is given a wider berth each time.
    pub strikes: u8,
    /// How long (ms) it has been on the glass since a letter last took it off, or since it
    /// first came on. At [`Rule::forgive_ms`] the strikes are forgiven.
    pub calm_ms: u32,
}

impl SeatState {
    /// A pet not yet on the glass, to come in from `edge`.
    #[must_use]
    #[inline]
    pub const fn offstage(edge: Edge) -> Self {
        Self {
            seat: Seat::Off(edge),
            caret: None,
            barred: [None; BARS],
            beaten_ms: 0,
            quiet_owed: false,
            strikes: 0,
            calm_ms: 0,
        }
    }

    /// The state after the page scrolled `up` rows under the pet (negative: down): a seat on
    /// the page rides it, and one carried past the pane's first or last row is off the glass.
    /// The seats it left and the caret it remembers ride too, and are forgotten where the page
    /// carried them off.
    #[must_use]
    #[inline]
    pub const fn ride(self, up: i32, rows: u16, cols: u16) -> Self {
        const fn rode(row: u16, up: i32, rows: u16) -> Option<u16> {
            let to = row as i64 - up as i64;
            if to < 0 || to >= rows as i64 {
                return None;
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "inside 0..rows, checked above"
            )]
            Some(to as u16)
        }
        let caret = match self.caret {
            Some((row, col)) => match rode(row, up, rows) {
                Some(row) => Some((row, col)),
                None => None,
            },
            None => None,
        };
        let mut barred = self.barred;
        let mut k = 0;
        while k < BARS {
            barred[k] = match barred[k] {
                Some(bar) => match rode(bar.row, up, rows) {
                    Some(row) => Some(Bar { row, ..bar }),
                    None => None,
                },
                None => None,
            };
            k += 1;
        }
        let rest = Self {
            caret,
            barred,
            ..self
        };
        let Seat::On { row, col, small } = self.seat else {
            return rest;
        };
        match rode(row, up, rows) {
            Some(row) => Self {
                seat: Seat::On { row, col, small },
                ..rest
            },
            None => {
                let edge = if (col as u32) * 2 < cols as u32 {
                    Edge::Left
                } else {
                    Edge::Right
                };
                Self {
                    seat: Seat::Off(edge),
                    beaten_ms: 0,
                    quiet_owed: true,
                    ..rest
                }
            }
        }
    }
}

/// Why [`decide`] answered as it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The seat is still the pet's.
    Kept,
    /// A letter landed on the seat, or the pane shrank past it: the pet is not drawn there this
    /// frame and is off the glass.
    Yielded,
    /// The caret, the writing's next cells or the hand's own letters (a paste, a completion, a
    /// suggestion along the caret's line) came into the seat, or the page scrolled its rows
    /// under it ([`Scene::scrolled`]): the pet leaves it this frame for the best seat there is,
    /// or for off the glass where it was chased out of [`BARS`] runs of seats within one dwell.
    GaveWay,
    /// The page is another page ([`Scene::new_page`]) and the seat is no seat on it: the pet
    /// goes this frame to the best seat the new page has.
    Switched,
    /// Another seat stayed better for the dwell.
    Moved,
    /// A seat it can walk to from an edge, out of the writing's reach, stayed better, with the
    /// rows about the caret quiet long enough where it owes that: it comes on the glass.
    Returned,
}

/// A seat decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// The state to carry to the next asking; its `seat` is the decision.
    pub state: SeatState,
    /// Why.
    pub why: Why,
    /// Whether another seat is better by the margin and the pet has not left for it yet: it
    /// leaves once that has lasted the dwell, the scene no longer holds it and the quiet it
    /// owes has passed. A host that draws on demand asks again by then.
    pub better: bool,
}

/// The way from one seat to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// The two are one seat.
    Stay,
    /// Along its row on its feet: every cell its box passes over is clear.
    Walk,
    /// One bound through the air, drawn whole, from the one seat to the other.
    Leap,
    /// On its feet along its row and out through the edge.
    Exit(Edge),
    /// In through the edge and along the seat's row on its feet.
    Enter(Edge),
    /// No way on its feet: it is not drawn on the way (a short fade out, or in).
    Cut,
}

/// A candidate's rank: the least is the best. Cost first; then a seat on the page before one
/// off it, the caret's row before the row over it before the rows under it, the whole drawing
/// before the small, the nearer column to the far end of the caret's row's free lead (on another
/// row, to the cell past the caret), the lower column.
type Rank = (u32, u16, bool, u16, u16);

#[inline]
const fn caret_i(scene_caret: (u16, u16)) -> (i32, i32) {
    (scene_caret.0 as i32, scene_caret.1 as i32)
}

/// The first column past the hand's line on the caret's row: the cell past the caret, or the
/// cell past the last letter of the line that runs on from the caret (a shell's suggestion, the
/// rest of a line the caret went back into), its words joined across [`Rule::line_join`] blank
/// cells. A seat's distance along the caret's row is counted from here and so is the free lead:
/// a seat past a suggestion's end is beside the line, as one past the caret is on a bare line.
/// Counted from the caret, the seat past a suggestion cost its whole length, off the glass cost
/// less, and the pet left the glass at the first suggestion of every command.
#[must_use]
pub fn line_past<P: Page>(rule: &Rule, scene: &Scene<'_, P>) -> i32 {
    let (cr, cc) = caret_i(scene.caret);
    let mut past = cc + 1;
    if scene.caret.0 >= scene.page.rows() {
        return past;
    }
    let cols = i32::from(scene.page.cols());
    let mut c = past;
    while c < cols && c - past <= i32::from(rule.line_join) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "inside the pane: cc < c < cols"
        )]
        if scene.page.lettered(cr as u16, c as u16) {
            past = c + 1;
        }
        c += 1;
    }
    past
}

/// Whether the writing's next cells take `(row, col)` (the caret's own cell is not counted):
/// [`Scene::ahead`] of them, and never fewer than [`Rule::gap`], past the caret; and where the
/// line runs on from the caret ([`line_past`], `past`), every cell of it, the blanks between
/// its words too (A GAP IN THE LINE IS THE LINE'S), and as many past its end, which a key
/// typed into the line pushes on.
#[inline]
fn claimed<P: Page>(rule: &Rule, scene: &Scene<'_, P>, past: i32, row: i32, col: i32) -> bool {
    let ahead = i32::from(scene.ahead.max(rule.gap));
    if ahead == 0 {
        return false;
    }
    let (cr, cc) = caret_i(scene.caret);
    if past > cc + 1 && row == cr && col > cc && col < past + ahead {
        return true;
    }
    let cols = i32::from(scene.page.cols());
    let rows = i32::from(scene.page.rows());
    // In 64 bits: a seat 65,535 rows from the caret on a pane as wide is past 32.
    let d = i64::from(row - cr) * i64::from(cols) + i64::from(col - cc);
    if d > 0 && d <= i64::from(ahead) {
        return true;
    }
    // On the pane's last row the wrap scrolls the page and the writing goes on from that row's
    // first cell.
    cr == rows - 1 && row == cr && col < cc + ahead - (cols - 1)
}

/// What a cell is asked to be clear of, each more than the one before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Of {
    /// Letters: the cell is inside the pane's columns, not under its last row, and not
    /// lettered. A row over the pane's first is clear: a body on the first row is cut by the
    /// pane's edge.
    Letters,
    /// Letters and the caret's cell: what a way passes over.
    Caret,
    /// Letters, the caret's cell and the cells the writing is about to take, the hand's line
    /// ending at the column given ([`line_past`]): what a seat covers.
    Writing(i32),
}

/// Whether `(row, col)` is clear `of` what is asked.
#[inline]
fn clear<P: Page>(rule: &Rule, scene: &Scene<'_, P>, row: i32, col: i32, of: Of) -> bool {
    let (rows, cols) = (i32::from(scene.page.rows()), i32::from(scene.page.cols()));
    if col < 0 || col >= cols || row >= rows {
        return false;
    }
    if row < 0 {
        return true;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "inside the pane, checked above"
    )]
    let (r, c) = (row as u16, col as u16);
    if scene.page.lettered(r, c) {
        return false;
    }
    match of {
        Of::Letters => true,
        Of::Caret => (row, col) != caret_i(scene.caret),
        Of::Writing(past) => {
            (row, col) != caret_i(scene.caret) && !claimed(rule, scene, past, row, col)
        }
    }
}

/// Whether every row of a box `size` tall standing on `row` is clear at `col`.
#[inline]
fn column_clear<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    (row, size): (i32, Size),
    col: i32,
    of: Of,
) -> bool {
    let mut k = 0;
    while k < i32::from(size.rows) {
        if !clear(rule, scene, row - k, col, of) {
            return false;
        }
        k += 1;
    }
    true
}

/// Whether every cell a box `size` standing on `row` covers between the columns `from` and
/// `to` (end exclusive) is clear.
fn span_clear<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    on: (i32, Size),
    (from, to): (i32, i32),
    of: Of,
) -> bool {
    let mut c = from;
    while c < to {
        if !column_clear(rule, scene, on, c, of) {
            return false;
        }
        c += 1;
    }
    true
}

/// The box of a seat on the page: its feet row, its first column and its size. `None` for a
/// seat off the glass, one under the pane's last row, and a small one of a body with no small
/// drawing.
#[inline]
fn box_of<P: Page>(scene: &Scene<'_, P>, seat: Seat) -> Option<(i32, i32, Size)> {
    let Seat::On { row, col, small } = seat else {
        return None;
    };
    let size = scene.body.size(small)?;
    if row >= scene.page.rows() || size.cols == 0 || size.rows == 0 {
        return None;
    }
    Some((i32::from(row), i32::from(col), size))
}

/// Whether a seat on the page has its box inside the pane and clear `of` what is asked.
#[inline]
fn box_clear<P: Page>(rule: &Rule, scene: &Scene<'_, P>, seat: Seat, of: Of) -> bool {
    box_of(scene, seat).is_some_and(|(row, col, size)| {
        span_clear(
            rule,
            scene,
            (row, size),
            (col, col + i32::from(size.cols)),
            of,
        )
    })
}

/// Whether `seat` is a seat now: on the page, a box inside the pane that covers no lettered
/// cell, not the caret's cell and none of the cells the writing is about to take; off it,
/// always.
#[must_use]
pub fn legal<P: Page>(rule: &Rule, scene: &Scene<'_, P>, seat: Seat) -> bool {
    !seat.on_page() || box_clear(rule, scene, seat, Of::Writing(line_past(rule, scene)))
}

/// What a seat on the page costs, whatever lies under it, the hand's line ending at the column
/// `line` ([`line_past`]).
#[inline]
fn on_cost<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    (row, col): (i32, i32),
    size: Size,
    small: bool,
    line: i32,
) -> u32 {
    let (cr, cc) = caret_i(scene.caret);
    let past = cc + 1;
    let dy = row - cr;
    let ahead_of_it = col >= past;
    let dx = if ahead_of_it {
        if dy == 0 {
            (col - line - i32::from(rule.lead)).max(0)
        } else {
            col - past
        }
    } else {
        (cc - (col + i32::from(size.cols))).max(0)
    };
    let rows = match dy.unsigned_abs() {
        n @ 0..=2 => n * rule.row,
        n => 2 * rule.row + (n - 2) * rule.far_row,
    };
    let mut cost = dx.unsigned_abs() + rows;
    if dy > 0 && scene.below_is_next {
        cost += rule.below;
    }
    if !ahead_of_it {
        cost += rule.behind;
    }
    if small {
        cost += rule.small;
    }
    cost
}

/// The seat off the glass a caret asks for, and its cost: through the nearer edge, the right
/// one where they are as near.
#[inline]
fn off_seat<P: Page>(rule: &Rule, scene: &Scene<'_, P>) -> (Seat, u32) {
    let cc = u32::from(scene.caret.1);
    let right = u32::from(scene.page.cols())
        .saturating_sub(1)
        .saturating_sub(cc);
    if right <= cc {
        (Seat::Off(Edge::Right), right + rule.off)
    } else {
        (Seat::Off(Edge::Left), cc + rule.off)
    }
}

/// What `seat` costs now.
#[must_use]
pub fn cost<P: Page>(rule: &Rule, scene: &Scene<'_, P>, seat: Seat) -> u32 {
    match seat {
        Seat::Off(_) => off_seat(rule, scene).1,
        Seat::On { row, col, small } => {
            let size = scene.body.size(small).unwrap_or(scene.body.whole);
            on_cost(
                rule,
                scene,
                (i32::from(row), i32::from(col)),
                size,
                small,
                line_past(rule, scene),
            )
        }
    }
}

/// The order the rows about the caret are read in: the caret's, the rows over it nearest
/// first, then the rows under it nearest first. `k` counts from 0; `None` past the last.
#[inline]
const fn row_at(rule: &Rule, k: u16) -> Option<i32> {
    if k == 0 {
        Some(0)
    } else if k <= rule.rows_up {
        Some(-(k as i32))
    } else if k - rule.rows_up <= rule.rows_down {
        Some((k - rule.rows_up) as i32)
    } else {
        None
    }
}

/// Whether a fence ([`Page::fence`]) lies between the caret's row `cr` and a seat's feet on
/// `row`, that row included: the seat is then across a program's rule from the line.
#[inline]
fn fenced<P: Page>(scene: &Scene<'_, P>, cr: i32, row: i32) -> bool {
    let (mut r, last) = if row < cr {
        (row, cr - 1)
    } else {
        (cr + 1, row)
    };
    while r <= last {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "between two rows of the pane"
        )]
        if r >= 0 && scene.page.fence(r as u16) {
            return true;
        }
        r += 1;
    }
    false
}

/// The best seat and its cost, among every seat on the rows about the caret and the seat off
/// the glass; the `barred` seats are left out, a row across a fence from the caret has none
/// ([`fenced`]), and `walk_in` leaves out every seat on the page no walk along its row from a
/// side edge reaches.
fn search<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    barred: &[Option<Bar>; BARS],
    walk_in: bool,
) -> (Seat, u32) {
    let (off, off_cost) = off_seat(rule, scene);
    let mut found = off;
    let mut rank: Rank = (off_cost, u16::MAX, false, 0, 0);
    let (cr, cc) = caret_i(scene.caret);
    let (rows, cols) = (i32::from(scene.page.rows()), i32::from(scene.page.cols()));
    let line = line_past(rule, scene);
    let of = Of::Writing(line);
    let mut k = 0u16;
    while let Some(dy) = row_at(rule, k) {
        let order = k;
        k += 1;
        let row = cr + dy;
        if row < 0 || row >= rows || fenced(scene, cr, row) {
            continue;
        }
        for small in [false, true] {
            let Some(size) = scene.body.size(small) else {
                continue;
            };
            let w = i32::from(size.cols);
            if w == 0 || size.rows == 0 || w > cols {
                continue;
            }
            // The first and the last column of the row a box may not stand over: a seat short
            // of the first is walked to from the left edge, one past the last from the right.
            let (mut first_shut, mut last_shut) = (cols, -1);
            if walk_in {
                let mut c = 0;
                while c < cols {
                    if !column_clear(rule, scene, (row, size), c, of) {
                        if first_shut == cols {
                            first_shut = c;
                        }
                        last_shut = c;
                    }
                    c += 1;
                }
            }
            let mut run = 0;
            let mut c = 0;
            while c < cols {
                run = if column_clear(rule, scene, (row, size), c, of) {
                    run + 1
                } else {
                    0
                };
                c += 1;
                if run < w {
                    continue;
                }
                let col = c - w;
                if walk_in && !(c <= first_shut || col > last_shut) {
                    continue;
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "inside the pane: 0 <= col < cols and 0 <= row < rows"
                )]
                let seat = Seat::On {
                    row: row as u16,
                    col: col as u16,
                    small,
                };
                if barred.iter().flatten().any(|b| b.holds(seat)) {
                    continue;
                }
                // Among seats of one cost on the caret's row, the one at the far end of the
                // free lead: the first keys then find it out of their way, where the nearest
                // one would be sent on by the first of them.
                let target = if dy == 0 {
                    line + i32::from(rule.lead)
                } else {
                    cc + 1
                };
                let near = (col - target).unsigned_abs();
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a distance inside a pane of at most 65535 columns"
                )]
                let this: Rank = (
                    on_cost(rule, scene, (row, col), size, small, line),
                    order,
                    small,
                    near as u16,
                    col as u16,
                );
                if this < rank {
                    rank = this;
                    found = seat;
                }
            }
        }
    }
    (found, rank.0)
}

/// THE ONE RULE: the best seat for the scene and its cost, with no memory of where the pet is.
#[must_use]
pub fn best<P: Page>(rule: &Rule, scene: &Scene<'_, P>) -> (Seat, u32) {
    search(rule, scene, &[None; BARS], false)
}

/// The side edge nearer a box.
#[inline]
fn nearer_edge(cols: u16, col: i32, size: Size) -> Edge {
    if col * 2 + i32::from(size.cols) < i32::from(cols) {
        Edge::Left
    } else {
        Edge::Right
    }
}

/// Whether `(row, col)` is a cell of the hand's own line: in writing order from the caret's
/// cell at the last asking (`was`) to the caret's now, and on along the caret's row. A caret
/// that went back, or further down than the rule's rows reach, starts the line where it is
/// now.
#[inline]
fn written<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    was: Option<(u16, u16)>,
    (row, col): (i32, i32),
) -> bool {
    let now = scene.caret;
    let from = match was {
        Some(was) if was <= now && now.0 - was.0 <= rule.rows_down => was,
        _ => now,
    };
    row <= i32::from(now.0) && (row, col) >= (i32::from(from.0), i32::from(from.1))
}

/// Whether a seat on the page has its box inside the pane with no letter under it but the
/// hand's own line ([`written`]).
fn only_writing_under<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    was: Option<(u16, u16)>,
    seat: Seat,
) -> bool {
    let Some((row, col, size)) = box_of(scene, seat) else {
        return false;
    };
    if col + i32::from(size.cols) > i32::from(scene.page.cols()) {
        return false;
    }
    let mut c = col;
    while c < col + i32::from(size.cols) {
        let mut k = 0;
        while k < i32::from(size.rows) {
            let at = (row - k, c);
            if !clear(rule, scene, at.0, at.1, Of::Letters) && !written(rule, scene, was, at) {
                return false;
            }
            k += 1;
        }
        c += 1;
    }
    true
}

/// Bars the seat `from` as the pet leaves it for `to`, and with it the seats between them where
/// the two are along one row. `false` where the bars are full of other runs and none may be
/// dropped (`keep`): the pet has then been chased out of [`BARS`] of them within the dwell.
fn bar(barred: &mut [Option<Bar>; BARS], from: Seat, to: Seat, keep: bool) -> bool {
    let (
        Seat::On { row, col, small },
        Seat::On {
            row: to_row,
            col: to_col,
            small: to_small,
        },
    ) = (from, to)
    else {
        // To or from off the glass: a way back on takes a dwell at least.
        return true;
    };
    let (first, last) = if to_row == row && to_small == small && to_col > col {
        (col, to_col - 1)
    } else if to_row == row && to_small == small && to_col < col {
        (to_col + 1, col)
    } else {
        (col, col)
    };
    for b in barred.iter_mut().flatten() {
        let touches = first <= b.last.saturating_add(1) && b.first <= last.saturating_add(1);
        if b.row == row && b.small == small && touches {
            b.first = b.first.min(first);
            b.last = b.last.max(last);
            b.ago_ms = 0;
            return true;
        }
    }
    let new = Some(Bar {
        row,
        small,
        first,
        last,
        ago_ms: 0,
    });
    if let Some(free) = barred.iter_mut().find(|b| b.is_none()) {
        *free = new;
        return true;
    }
    if keep {
        return false;
    }
    if let Some(oldest) = barred
        .iter_mut()
        .max_by_key(|b| b.map_or(u32::MAX, |b| b.ago_ms))
    {
        *oldest = new;
    }
    true
}

/// THE SEAT DECISION: where the pet in `state` sits now, `dt_ms` after it was last asked.
///
/// A seat a letter landed on (or the pane shrank past) is left at once for off the glass
/// ([`Why::Yielded`]): the pet is not drawn over the letter. A seat the caret, the writing's
/// next cells or the hand's own line came into, or the page scrolled its rows under
/// ([`Scene::scrolled`]: those letters came up with the page, none landed), is left at once for
/// the best seat there is ([`Why::GaveWay`]), every seat it left within the dwell left out;
/// chased out of [`BARS`] runs of seats within one dwell, it leaves the glass. On a page that
/// is another page ([`Scene::new_page`]) a seat that is none there is left at once for the new
/// page's best ([`Why::Switched`]). A seat that is still one is left only once another has
/// been better by the margin for the dwell, and never while the scene holds the pet
/// ([`Scene::hold`]).
///
/// A pet that left the glass comes back only by walking in from an edge, only to a seat the
/// writing cannot reach within a dwell ([`Rule::reach`]), and only once the rows about the
/// caret have been quiet for [`Rule::quiet_owed_ms`]: the same quiet whether a letter took it
/// off or it walked off for the writing, and twice as long after each further letter until
/// [`Rule::forgive_ms`] on the glass forgives them.
///
/// So the pet never goes back to a seat on the page within the dwell of leaving it, whatever
/// seats lie between; and a pet that came on the glass is not sent off it within the dwell by a
/// hand that takes [`Rule::reach`] cells or fewer in that time.
#[must_use]
pub fn decide<P: Page>(
    rule: &Rule,
    scene: &Scene<'_, P>,
    state: SeatState,
    dt_ms: u32,
) -> Decision {
    let cur = state.seat;
    let was = if scene.new_page { None } else { state.caret };
    let mut barred = if scene.new_page {
        [None; BARS]
    } else {
        state.barred
    };
    for slot in &mut barred {
        if let Some(bar) = slot {
            bar.ago_ms = bar.ago_ms.saturating_add(dt_ms);
            if bar.ago_ms >= rule.dwell_ms {
                *slot = None;
            }
        }
    }
    let calm_ms = if cur.on_page() {
        state.calm_ms.saturating_add(dt_ms)
    } else {
        state.calm_ms
    };
    let strikes = if calm_ms >= rule.forgive_ms {
        0
    } else {
        state.strikes
    };
    let now = SeatState {
        caret: Some(scene.caret),
        barred,
        strikes,
        calm_ms,
        ..state
    };
    let kept = |beaten_ms: u32, better: bool| Decision {
        state: SeatState { beaten_ms, ..now },
        why: Why::Kept,
        better,
    };
    let changed = |seat: Seat, why: Why, barred: [Option<Bar>; BARS]| Decision {
        state: SeatState {
            seat,
            barred,
            beaten_ms: 0,
            quiet_owed: !seat.on_page(),
            ..now
        },
        why,
        better: false,
    };
    if !legal(rule, scene, cur) {
        if scene.new_page {
            let (seat, _) = search(rule, scene, &barred, false);
            return changed(seat, Why::Switched, barred);
        }
        // Letters a scroll brought under a box that is still inside the pane did not land on
        // it.
        let scrolled_under = scene.scrolled
            && box_of(scene, cur).is_some_and(|(_, col, size)| {
                col + i32::from(size.cols) <= i32::from(scene.page.cols())
            });
        if !scrolled_under
            && !box_clear(rule, scene, cur, Of::Letters)
            && !only_writing_under(rule, scene, was, cur)
        {
            let edge = match cur {
                Seat::On { col, small, .. } => nearer_edge(
                    scene.page.cols(),
                    i32::from(col),
                    scene.body.size(small).unwrap_or(scene.body.whole),
                ),
                Seat::Off(edge) => edge,
            };
            let mut d = changed(Seat::Off(edge), Why::Yielded, barred);
            d.state.strikes = strikes.saturating_add(1);
            d.state.calm_ms = 0;
            return d;
        }
        let (mut seat, _) = search(rule, scene, &barred, false);
        if !bar(&mut barred, cur, seat, true) {
            seat = off_seat(rule, scene).0;
        }
        return changed(seat, Why::GaveWay, barred);
    }
    let from_off = !cur.on_page();
    let (seat, seat_cost) = if from_off {
        // A way back on the glass ends out of the hand's reach, and the nearest seat there on
        // the caret's row costs nothing: the pet comes in along the line it will rest on, and
        // walks on to the end of the free lead a dwell later.
        let reached = Scene {
            ahead: scene.ahead.max(rule.reach),
            ..*scene
        };
        let coming = Rule {
            lead: rule.lead.max(rule.reach),
            ..*rule
        };
        search(&coming, &reached, &barred, true)
    } else {
        search(rule, scene, &barred, false)
    };
    let better = !seat.same(cur) && seat_cost.saturating_add(rule.margin) <= cost(rule, scene, cur);
    if !better {
        return kept(0, false);
    }
    let beaten_ms = state.beaten_ms.saturating_add(dt_ms);
    let quiet = !(from_off && state.quiet_owed) || scene.quiet_ms >= rule.quiet_owed_ms(strikes);
    if scene.hold || beaten_ms < rule.dwell_ms || !quiet {
        return kept(beaten_ms, true);
    }
    bar(&mut barred, cur, seat, false);
    changed(
        seat,
        if from_off { Why::Returned } else { Why::Moved },
        barred,
    )
}

/// THE ONE PATH: the way from the seat `from` to the seat `to` on this page.
///
/// A walk keeps to one row and one drawing and passes over clear cells only, from the box it
/// leaves to the box it takes; anything else between two seats on the page is one leap. To or
/// from off the glass it is a walk along the seat's row through a side edge, the edge named
/// where the way to it is clear and else the other; with neither clear the pet is not drawn on
/// the way. The cells the writing is about to take are no obstacle to a way: they hold nothing
/// yet.
#[must_use]
pub fn path<P: Page>(rule: &Rule, scene: &Scene<'_, P>, from: Seat, to: Seat) -> Path {
    let cols = i32::from(scene.page.cols());
    let through = |(row, col, size): (i32, i32, Size), first: Edge| {
        let open = |edge: Edge| {
            let span = match edge {
                Edge::Left => (0, col + i32::from(size.cols)),
                Edge::Right => (col, cols),
            };
            span_clear(rule, scene, (row, size), span, Of::Caret)
        };
        let other = match first {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
        };
        if open(first) {
            Some(first)
        } else if open(other) {
            Some(other)
        } else {
            None
        }
    };
    match (from, to) {
        _ if from.same(to) => Path::Stay,
        (Seat::On { .. }, Seat::On { .. }) => match (box_of(scene, from), box_of(scene, to)) {
            (Some((r0, c0, s0)), Some((r1, c1, s1)))
                if r0 == r1
                    && s0 == s1
                    && span_clear(
                        rule,
                        scene,
                        (r0, s0),
                        (c0.min(c1), c0.max(c1) + i32::from(s0.cols)),
                        Of::Caret,
                    ) =>
            {
                Path::Walk
            }
            _ => Path::Leap,
        },
        (Seat::On { .. }, Seat::Off(edge)) => box_of(scene, from)
            .and_then(|b| through(b, edge))
            .map_or(Path::Cut, Path::Exit),
        (Seat::Off(_), Seat::On { .. }) => box_of(scene, to)
            .and_then(|b| through(b, nearer_edge(scene.page.cols(), b.1, b.2)))
            .map_or(Path::Cut, Path::Enter),
        (Seat::Off(_), Seat::Off(_)) => Path::Stay,
    }
}

#[cfg(test)]
#[path = "pet_seat_tests.rs"]
mod tests;
