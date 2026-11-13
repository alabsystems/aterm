// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A page a test owns.
#[derive(Clone, Debug)]
struct Grid {
    rows: u16,
    cols: u16,
    ink: Vec<bool>,
    /// The rows drawn as a rule across the pane.
    fences: Vec<u16>,
}

impl Grid {
    fn blank(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            ink: vec![false; usize::from(rows) * usize::from(cols)],
            fences: Vec::new(),
        }
    }

    fn set(&mut self, row: u16, col: u16, on: bool) {
        self.ink[usize::from(row) * usize::from(self.cols) + usize::from(col)] = on;
    }

    fn fill_row(&mut self, row: u16, cols: std::ops::Range<u16>) {
        for c in cols {
            self.set(row, c, true);
        }
    }

    /// `text` on `row` from its first column: every cell but a space's is lettered.
    fn write(&mut self, row: u16, text: &str) {
        for (c, ch) in text.chars().enumerate() {
            self.set(row, u16::try_from(c).unwrap(), ch != ' ');
        }
    }

    /// A rule drawn across `row`.
    fn rule(&mut self, row: u16) {
        self.fill_row(row, 0..self.cols);
        self.fences.push(row);
    }
}

impl Page for Grid {
    fn rows(&self) -> u16 {
        self.rows
    }

    fn cols(&self) -> u16 {
        self.cols
    }

    fn lettered(&self, row: u16, col: u16) -> bool {
        self.ink[usize::from(row) * usize::from(self.cols) + usize::from(col)]
    }

    fn fence(&self, row: u16) -> bool {
        self.fences.contains(&row)
    }
}

/// The cat's and the dog's body on the trail gallery's 14 x 26 px cells: 5.2 cells across and
/// 1.7 rows tall whole, 2.8 by 0.92 fitted to a row (`kitty_pet`'s `art_cols`, `ROW_FIT`).
const PET: Body = Body::of_art(5.2, 1.7, 0.54);

const RULE: Rule = Rule::DEFAULT;

fn scene(page: &Grid, caret: (u16, u16)) -> Scene<'_, Grid> {
    Scene {
        page,
        caret,
        body: PET,
        ahead: 0,
        below_is_next: false,
        quiet_ms: 60_000,
        hold: false,
        new_page: false,
        scrolled: false,
    }
}

fn on(row: u16, col: u16) -> Seat {
    Seat::On {
        row,
        col,
        small: false,
    }
}

fn small(row: u16, col: u16) -> Seat {
    Seat::On {
        row,
        col,
        small: true,
    }
}

fn sat(seat: Seat) -> SeatState {
    SeatState {
        seat,
        ..SeatState::offstage(Edge::Right)
    }
}

/// The seats a state bars: none.
const NO_BARS: [Option<Bar>; BARS] = [None; BARS];

/// Whether a state bars `seat`.
fn bars(state: &SeatState, seat: Seat) -> bool {
    state.barred.iter().flatten().any(|b| b.holds(seat))
}

/// A blank page of any size, held as its size alone.
struct Blank(u16, u16);

impl Page for Blank {
    fn rows(&self) -> u16 {
        self.0
    }

    fn cols(&self) -> u16 {
        self.1
    }

    fn lettered(&self, _row: u16, _col: u16) -> bool {
        false
    }
}

/// Every cell a seat's box covers inside the pane.
fn box_cells(body: Body, rows: u16, seat: Seat) -> Vec<(i32, i32)> {
    let Seat::On { row, col, small } = seat else {
        return Vec::new();
    };
    let size = if small {
        body.small.expect("a small seat of a body drawn small")
    } else {
        body.whole
    };
    assert!(row < rows, "{seat:?} stands inside the pane's rows");
    let mut cells = Vec::new();
    for r in 0..i32::from(size.rows) {
        for c in 0..i32::from(size.cols) {
            if i32::from(row) - r >= 0 {
                cells.push((i32::from(row) - r, i32::from(col) + c));
            }
        }
    }
    cells
}

/// THE PROPERTY, read without the rule's own readers: a seat on the page lies inside the pane's
/// columns, on no lettered cell and not on the caret's cell.
fn assert_clear(s: &Scene<'_, Grid>, seat: Seat, what: &str) {
    for (r, c) in box_cells(s.body, s.page.rows, seat) {
        assert!(
            c >= 0 && c < i32::from(s.page.cols),
            "{what}: {seat:?} reaches column {c} of {}",
            s.page.cols
        );
        let (r, c) = (u16::try_from(r).unwrap(), u16::try_from(c).unwrap());
        assert!(
            !s.page.lettered(r, c),
            "{what}: {seat:?} covers the lettered cell ({r}, {c})"
        );
        assert!(
            (r, c) != s.caret,
            "{what}: {seat:?} covers the caret's cell {:?}",
            s.caret
        );
    }
}

/// Whether a seat on the page has a letter under its box, or is not inside the pane.
fn on_letters_or_out(s: &Scene<'_, Grid>, seat: Seat) -> bool {
    let Seat::On { row, small, .. } = seat else {
        return false;
    };
    if row >= s.page.rows || (small && s.body.small.is_none()) {
        return true;
    }
    box_cells(s.body, s.page.rows, seat)
        .into_iter()
        .any(|(r, c)| {
            c >= i32::from(s.page.cols)
                || s.page
                    .lettered(u16::try_from(r).unwrap(), u16::try_from(c).unwrap())
        })
}

#[test]
fn the_body_s_boxes_are_the_whole_cells_its_art_touches() {
    assert_eq!(PET.whole, Size { cols: 6, rows: 2 });
    assert_eq!(PET.small, Some(Size { cols: 3, rows: 1 }));
    assert_eq!(Body::of_art(3.0, 1.0, 0.0).whole, Size { cols: 3, rows: 1 });
    assert_eq!(Body::of_art(3.0, 1.0, 0.0).small, None);
    assert_eq!(Body::of_art(0.2, 0.0, 1.0).whole, Size { cols: 1, rows: 1 });
    assert_eq!(Body::of_art(0.2, 0.0, 1.0).small, None);
}

#[test]
fn a_bit_page_reads_as_the_cells_it_was_written_from() {
    let mut g = Grid::blank(5, 130);
    let mut rng = Rng(7);
    for cell in &mut g.ink {
        *cell = rng.below(3) == 0;
    }
    let stride = BitPage::stride(g.cols);
    assert_eq!(stride, 3);
    let mut words = vec![0u64; usize::from(g.rows) * stride];
    for r in 0..g.rows {
        for c in 0..g.cols {
            if g.lettered(r, c) {
                words[usize::from(r) * stride + usize::from(c) / 64] |= 1 << (c % 64);
            }
        }
    }
    let bits = BitPage::new(g.rows, g.cols, &words).expect("words for every row");
    assert!(BitPage::new(g.rows, g.cols, &words[1..]).is_none());
    for r in 0..g.rows {
        for c in 0..g.cols {
            assert_eq!(bits.lettered(r, c), g.lettered(r, c), "({r}, {c})");
        }
    }
    let a = Scene {
        page: &bits,
        caret: (2, 40),
        body: PET,
        ahead: 2,
        below_is_next: true,
        quiet_ms: 0,
        hold: false,
        new_page: false,
        scrolled: false,
    };
    let mut b = scene(&g, (2, 40));
    b.ahead = 2;
    b.below_is_next = true;
    assert_eq!(best(&RULE, &a), best(&RULE, &b));
}

#[test]
fn on_a_blank_page_the_seat_is_at_the_far_end_of_the_free_lead() {
    let g = Grid::blank(14, 72);
    // Every seat from the gap to the lead's end costs nothing; the seat is the one of them the
    // first keys reach last.
    let s = scene(&g, (4, 2));
    assert_eq!(best(&RULE, &s), (on(4, 7), 0));
    assert!(legal(&RULE, &s, on(4, 4)));
    assert_eq!(cost(&RULE, &s, on(4, 4)), 0);
    assert_eq!(best(&RULE, &scene(&g, (0, 0))), (on(0, 5), 0));
    // The caret's next cell is no seat, so a first key moves the caret beside the pet.
    assert!(!legal(&RULE, &s, on(4, 3)));
    let no_gap = Rule { gap: 0, ..RULE };
    assert!(legal(&no_gap, &s, on(4, 3)));
}

#[test]
fn with_no_room_past_the_caret_the_seat_is_behind_it() {
    let g = Grid::blank(14, 72);
    // Six cells past column 66 and its gap do not fit 72; the seat behind ends where the
    // caret's cell begins.
    assert_eq!(best(&RULE, &scene(&g, (4, 66))), (on(4, 60), RULE.behind));
    assert_eq!(best(&RULE, &scene(&g, (4, 64))), (on(4, 66), 0));
}

#[test]
fn a_typed_line_at_the_wall_sends_the_seat_off_the_line() {
    // The line fills its row to the caret near the wall: behind is letters, past is the wall,
    // so the seat is over the line, its box ending over the caret's column.
    let mut g = Grid::blank(14, 72);
    g.fill_row(6, 0..66);
    let mut s = scene(&g, (6, 66));
    assert_eq!(best(&RULE, &s), (on(5, 66), RULE.row + RULE.behind));
    s.below_is_next = true;
    assert_eq!(best(&RULE, &s).0, on(5, 66));
    // With the row over the line lettered too, the whole body two rows under the line costs
    // less than the small drawing in the line's last cells.
    g.fill_row(5, 0..72);
    let mut s = scene(&g, (6, 66));
    assert_eq!(best(&RULE, &s), (on(8, 66), 2 * RULE.row + RULE.behind));
    s.below_is_next = true;
    assert_eq!(
        best(&RULE, &s),
        (on(8, 66), 2 * RULE.row + RULE.below + RULE.behind)
    );
    // With no two blank rows under the line either, the small drawing in the line's last cells.
    g.fill_row(7, 0..72);
    g.fill_row(9, 0..72);
    assert_eq!(best(&RULE, &scene(&g, (6, 66))), (small(6, 69), RULE.small));
}

#[test]
fn under_a_lettered_row_the_whole_body_a_row_down_comes_before_the_small_one() {
    let mut g = Grid::blank(14, 72);
    g.fill_row(3, 0..72);
    g.set(4, 0, true);
    let mut s = scene(&g, (4, 2));
    // Whole on row 5: its box covers rows 4 and 5, its head in the caret's row past the gap.
    assert_eq!(best(&RULE, &s), (on(5, 4), 1 + RULE.row));
    s.below_is_next = true;
    assert_eq!(best(&RULE, &s), (on(5, 4), 1 + RULE.row + RULE.below));
    // A lettered row under the caret's too: the whole body three rows down, clear of both, is
    // a seat, and it is not beside the line: the small drawing on the caret's own row comes
    // before it (the trail film's `steam`, where the pet sat three rows under the typed line
    // in every frame). With the caret's row taken to its end, the nearest seat is the small
    // drawing two rows down.
    g.fill_row(5, 0..72);
    let s = scene(&g, (4, 2));
    assert_eq!(best(&RULE, &s), (small(4, 7), RULE.small));
    assert!(legal(&RULE, &s, on(7, 3)));
    assert_eq!(cost(&RULE, &s, on(7, 3)), 2 * RULE.row + RULE.far_row);
    const { assert!(RULE.small < 2 * RULE.row + RULE.far_row) };
    g.fill_row(4, 3..72);
    assert_eq!(
        best(&RULE, &scene(&g, (4, 2))),
        (small(6, 3), 2 * RULE.row + RULE.small)
    );
}

#[test]
fn a_whole_seat_a_short_way_along_comes_before_the_small_one_beside_the_caret() {
    let mut g = Grid::blank(6, 72);
    g.fill_row(5, 0..72);
    g.fill_row(4, 0..2);
    // The row over the caret's ends at column 10: a whole seat past it costs its distance less
    // the lead, 11 - 3 - 4 = 4, under the small drawing's 9.
    g.fill_row(3, 0..11);
    assert_eq!(best(&RULE, &scene(&g, (4, 2))), (on(4, 11), 4));
    // Ending at column 16 it costs 17 - 3 - 4 = 10, and the small one beside the caret is the
    // seat.
    g.fill_row(3, 0..17);
    assert_eq!(best(&RULE, &scene(&g, (4, 2))), (small(4, 7), RULE.small));
}

#[test]
fn the_cells_the_writing_is_about_to_take_are_no_seat() {
    let mut g = Grid::blank(14, 72);
    let mut s = scene(&g, (4, 10));
    s.ahead = 3;
    assert!(legal(&RULE, &s, on(4, 14)) && !legal(&RULE, &s, on(4, 13)));
    assert_eq!(best(&RULE, &s), (on(4, 15), 0));
    // At the row's end the writing goes on from the next row's first cells.
    let mut s = scene(&g, (4, 70));
    s.ahead = 3;
    assert!(!legal(&RULE, &s, on(5, 0)));
    assert!(!legal(&RULE, &s, on(5, 1)));
    assert!(legal(&RULE, &s, on(5, 2)));
    // On the pane's last row the wrap scrolls: it goes on from that row's own first cells.
    let mut s = scene(&g, (13, 70));
    s.ahead = 3;
    assert!(!legal(&RULE, &s, small(13, 1)));
    assert!(legal(&RULE, &s, small(13, 2)));
    // A stream that takes most of the row of a typed line: the seat past it costs its distance
    // less the lead, and the seat on the row over the line, ground the stream never reaches, is
    // nearer.
    g.fill_row(4, 0..10);
    let mut s = scene(&g, (4, 10));
    s.ahead = 40;
    assert!(legal(&RULE, &s, on(4, 51)) && !legal(&RULE, &s, on(4, 50)));
    assert_eq!(cost(&RULE, &s, on(4, 51)), 40 - u32::from(RULE.lead));
    assert_eq!(best(&RULE, &s), (on(3, 11), RULE.row));
}

#[test]
fn a_full_screen_page_keeps_the_seat_off_the_rows_under_the_caret() {
    let mut g = Grid::blank(14, 72);
    g.fill_row(3, 0..72);
    let s = scene(&g, (4, 2));
    assert_eq!(best(&RULE, &s).0, on(5, 4));
    assert_eq!(
        best(&Rule::FULL_SCREEN, &s),
        (small(4, 7), RULE.small),
        "no seat under the line"
    );
}

/// KI-1107-4's page: every row over the last lettered in every cell, a prompt on the last row.
fn full_page_last_row(cols: u16) -> Grid {
    let mut g = Grid::blank(14, cols);
    for r in 0..13 {
        g.fill_row(r, 0..cols);
    }
    g.set(13, 0, true);
    g
}

/// One asking of a run: the seat, why, the way to it, and the caret's column.
type Step = (Seat, Why, Path, u16);

/// A rest by KI-1107-4's prompt and then a line typed along its last row to the wall, a key
/// every 50 ms: the changes of seat during the rest, and every step of the typing.
fn typed_to_the_wall(cols: u16) -> (Vec<Step>, Vec<Step>) {
    let mut g = full_page_last_row(cols);
    let mut state = SeatState::offstage(Edge::Right);
    let mut rest = Vec::new();
    for _ in 0..60 {
        let s = scene(&g, (13, 2));
        let d = decide(&RULE, &s, state, 16);
        if d.why != Why::Kept {
            rest.push((
                d.state.seat,
                d.why,
                path(&RULE, &s, state.seat, d.state.seat),
                2,
            ));
        }
        state = d.state;
    }
    let mut typing = Vec::new();
    for caret in 2..cols - 1 {
        g.set(13, caret, true);
        let mut s = scene(&g, (13, caret + 1));
        s.ahead = 3;
        s.quiet_ms = 0;
        let d = decide(&RULE, &s, state, 50);
        assert_clear(&s, d.state.seat, "typing to the wall");
        let way = path(&RULE, &s, state.seat, d.state.seat);
        typing.push((d.state.seat, d.why, way, caret + 1));
        state = d.state;
    }
    (rest, typing)
}

#[test]
fn at_the_wall_of_a_full_page_s_last_row_the_pet_walks_off_through_the_edge() {
    for cols in [69u16, 70, 71, 72, 73, 80] {
        let (rest, steps) = typed_to_the_wall(cols);
        // It came in from the right edge to the small seat out of a hand's reach, and a dwell
        // later walked on to the seat at the end of the free lead.
        assert_eq!(
            rest,
            [
                (
                    small(13, 3 + RULE.reach),
                    Why::Returned,
                    Path::Enter(Edge::Right),
                    2
                ),
                (small(13, 3 + RULE.lead), Why::Moved, Path::Walk, 2),
            ],
            "{cols} columns"
        );
        // It was on the glass from then until the wall left it no seat.
        let left = steps
            .iter()
            .rposition(|(seat, ..)| seat.on_page())
            .expect("it was on the glass");
        assert!(
            steps[..=left].iter().all(|(seat, ..)| seat.on_page()),
            "{cols} columns: on the glass from its entrance to the wall, with no gap"
        );
        // The first key finds the seat out of its way.
        assert_eq!(steps[0].1, Why::Kept, "{cols} columns");
        // Every step on the way is on its feet and for the writing, never for a letter under
        // it: a walk along the row.
        for (k, (seat, why, way, caret)) in steps.iter().enumerate() {
            assert!(
                matches!(way, Path::Stay | Path::Walk | Path::Exit(Edge::Right)),
                "{cols} columns, step {k}, caret {caret}: {way:?} to {seat:?}"
            );
            assert!(
                matches!(why, Why::Kept | Why::GaveWay),
                "{cols} columns, step {k}, caret {caret}: {why:?}"
            );
        }
        // The wall leaves no seat: it goes off through the right edge, on its feet, before the
        // writing reaches where it stood, and stays off while the hand writes.
        let (seat, why, way, caret) = steps[left + 1];
        assert_eq!(seat, Seat::Off(Edge::Right), "{cols} columns");
        assert_eq!(why, Why::GaveWay, "{cols} columns");
        assert_eq!(way, Path::Exit(Edge::Right), "{cols} columns");
        let Seat::On { col: stood, .. } = steps[left].0 else {
            unreachable!("on the glass")
        };
        assert!(
            caret < stood,
            "{cols} columns: it left with the caret at {caret}, short of its seat at {stood}"
        );
        assert!(
            steps[left + 1..].iter().all(|(seat, ..)| !seat.on_page()),
            "{cols} columns: no return while the hand writes"
        );
        assert!(
            left + 1 < steps.len() - 1,
            "{cols} columns: off before the line's end"
        );
    }
}

#[test]
fn a_seat_that_is_still_one_is_left_only_for_a_reason_that_lasts() {
    let g = Grid::blank(14, 72);
    let state = sat(on(4, 11));
    // The caret steps back: the seat is a cell further than the lead, inside the margin.
    let s = scene(&g, (4, 5));
    assert_eq!(best(&RULE, &s), (on(4, 10), 0));
    assert_eq!(cost(&RULE, &s, on(4, 11)), 1);
    for _ in 0..100 {
        let d = decide(&RULE, &s, state, 16);
        assert_eq!((d.state.seat, d.why), (on(4, 11), Why::Kept));
    }
    // The caret goes a row down: the seat by it is better by a row's cost, the margin, and the
    // pet leaves once that has lasted the dwell and not before.
    let s = scene(&g, (5, 10));
    let mut state = state;
    let mut at = 0;
    loop {
        let d = decide(&RULE, &s, state, 16);
        at += 16;
        state = d.state;
        if d.why != Why::Kept {
            assert_eq!((d.state.seat, d.why), (on(5, 15), Why::Moved));
            break;
        }
        assert_eq!(d.state.seat, on(4, 11));
        assert!(at < 1_000, "it leaves in the end");
    }
    assert!(
        (RULE.dwell_ms..RULE.dwell_ms + 16).contains(&at),
        "after {at} ms"
    );
    // A better seat that does not last resets the count.
    let mut state = sat(on(4, 11));
    for k in 0..200 {
        let caret = if k % 10 == 9 { (4, 9) } else { (5, 10) };
        let d = decide(&RULE, &scene(&g, caret), state, 16);
        assert_eq!(d.why, Why::Kept, "frame {k}");
        state = d.state;
    }
}

#[test]
fn a_held_pet_keeps_a_seat_that_is_still_one_and_yields_one_that_is_not() {
    let mut g = Grid::blank(14, 72);
    let mut s = scene(&g, (9, 40));
    s.hold = true;
    let mut state = sat(on(4, 11));
    for _ in 0..100 {
        let d = decide(&RULE, &s, state, 16);
        assert_eq!((d.state.seat, d.why), (on(4, 11), Why::Kept));
        state = d.state;
    }
    // Let go, it leaves at once: the reason lasted through the hold.
    s.hold = false;
    let d = decide(&RULE, &s, state, 16);
    assert_eq!((d.state.seat, d.why), (on(9, 45), Why::Moved));
    // The writing comes into a held pet's seat: it gives way that frame.
    let mut s = scene(&g, (4, 9));
    s.hold = true;
    s.ahead = 3;
    let d = decide(&RULE, &s, sat(on(4, 11)), 16);
    assert_eq!((d.state.seat, d.why), (on(4, 14), Why::GaveWay));
    assert_eq!(path(&RULE, &s, on(4, 11), d.state.seat), Path::Walk);
    // A letter under a held pet's head: it is off the glass that frame, through the nearer edge.
    g.set(3, 13, true);
    let mut s = scene(&g, (4, 2));
    s.hold = true;
    let d = decide(&RULE, &s, sat(on(4, 11)), 16);
    assert_eq!((d.state.seat, d.why), (Seat::Off(Edge::Left), Why::Yielded));
    assert!(d.state.quiet_owed);
    assert_eq!(d.state.strikes, 1);
}

#[test]
fn every_seat_left_within_the_dwell_is_barred_whatever_seats_lie_between() {
    let mut g = Grid::blank(3, 30);
    g.fill_row(0, 0..30);
    g.fill_row(2, 0..30);
    // Row 1 holds three small seats' room and no more: A at 4, B at 10, C at 16.
    g.fill_row(1, 0..4);
    g.fill_row(1, 7..10);
    g.fill_row(1, 13..16);
    g.fill_row(1, 19..30);
    let (a, b, c) = (small(1, 4), small(1, 10), small(1, 16));
    // The letters between the seats are read as a page's, not as the hand's line running on
    // from the caret ([`line_past`]), whose end would claim the cell past it: B's and C's first.
    const RULE: Rule = Rule {
        line_join: 0,
        ..Rule::DEFAULT
    };
    let s = scene(&g, (0, 29));
    assert!(legal(&RULE, &s, a) && legal(&RULE, &s, b) && legal(&RULE, &s, c));
    // The caret lands in A: the pet gives way to B, by a leap over the letters between.
    let s = scene(&g, (1, 5));
    let to_b = decide(&RULE, &s, sat(a), 16);
    assert_eq!((to_b.state.seat, to_b.why), (b, Why::GaveWay));
    assert!(bars(&to_b.state, a));
    assert_eq!(path(&RULE, &s, a, b), Path::Leap);
    // A frame later it lands in B: A is barred, so it goes on to C.
    let to_c = decide(&RULE, &scene(&g, (1, 11)), to_b.state, 16);
    assert_eq!((to_c.state.seat, to_c.why), (c, Why::GaveWay));
    assert!(bars(&to_c.state, a) && bars(&to_c.state, b));
    // A frame later it lands in C. A is the best seat left and the pet left it 32 ms ago, with
    // B and C between: it goes off the glass rather than back.
    let s = scene(&g, (1, 17));
    assert_eq!(best(&RULE, &s).0, b);
    let off = decide(&RULE, &s, to_c.state, 16);
    assert_eq!(off.why, Why::GaveWay);
    assert!(!off.state.seat.on_page(), "{:?}", off.state.seat);
    // The bars last the dwell from the leaving, each its own: just short of it A is barred
    // still, and at it A is a seat again.
    let short = decide(&RULE, &s, to_c.state, RULE.dwell_ms - 17);
    assert!(!short.state.seat.on_page(), "{:?}", short.state.seat);
    let late = decide(&RULE, &s, to_c.state, RULE.dwell_ms);
    assert_eq!((late.state.seat, late.why), (b, Why::GaveWay));
}

#[test]
fn a_walk_bars_the_seats_it_passed_and_a_pet_chased_out_of_four_runs_leaves_the_glass() {
    let g = Grid::blank(14, 72);
    // The writing sends the pet along its row: one run of seats, however many steps.
    let mut state = sat(on(4, 11));
    for caret in 9..30 {
        let mut s = scene(&g, (4, caret));
        s.ahead = 3;
        let d = decide(&RULE, &s, state, 16);
        assert!(d.state.seat.on_page(), "caret {caret}: {:?}", d.state.seat);
        state = d.state;
    }
    let runs: Vec<Bar> = state.barred.iter().flatten().copied().collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let Seat::On { col, .. } = state.seat else {
        unreachable!("on the glass")
    };
    assert_eq!((runs[0].row, runs[0].first, runs[0].last), (4, 11, col - 1));
    // Four runs of seats left within the dwell, and the caret comes into a fifth seat: there
    // is room all about, and the pet leaves the glass.
    let fresh = |row: u16| {
        Some(Bar {
            row,
            small: false,
            first: 50,
            last: 50,
            ago_ms: 10,
        })
    };
    let chased = SeatState {
        barred: [fresh(3), fresh(5), fresh(6), fresh(7)],
        ..sat(on(4, 11))
    };
    let s = scene(&g, (4, 12));
    let d = decide(&RULE, &s, chased, 16);
    assert_eq!(d.why, Why::GaveWay);
    assert!(!d.state.seat.on_page(), "{:?}", d.state.seat);
    assert!(d.state.quiet_owed);
    // One of the four is its own row's: the step joins that run, and it stays on the glass.
    let mut walked = chased;
    walked.barred[0] = Some(Bar {
        row: 4,
        small: false,
        first: 8,
        last: 10,
        ago_ms: 10,
    });
    let mut s = scene(&g, (4, 9));
    s.ahead = 3;
    let d = decide(&RULE, &s, walked, 16);
    assert_eq!((d.state.seat, d.why), (on(4, 14), Why::GaveWay));
    assert!(bars(&d.state, on(4, 8)) && bars(&d.state, on(4, 13)));
}

#[test]
fn a_pet_comes_back_only_on_its_feet_to_quiet_rows_and_out_of_the_hand_s_reach() {
    let mut g = Grid::blank(6, 40);
    g.fill_row(2, 0..10);
    let mut s = scene(&g, (2, 10));
    s.quiet_ms = RULE.return_quiet_ms - 1;
    // A pet that has not come on yet owes no quiet: it comes on after the dwell, to the seat
    // nearest the caret that a hand's reach past the caret leaves clear.
    let mut state = SeatState::offstage(Edge::Right);
    let mut at = 0;
    let out_of_reach = on(2, 11 + RULE.reach);
    loop {
        let d = decide(&RULE, &s, state, 16);
        at += 16;
        if d.why != Why::Kept {
            assert_eq!((d.state.seat, d.why), (out_of_reach, Why::Returned));
            assert_eq!(
                path(&RULE, &s, state.seat, d.state.seat),
                Path::Enter(Edge::Right)
            );
            state = d.state;
            break;
        }
        state = d.state;
        assert!(at < 1_000, "it comes on in the end");
    }
    assert!((RULE.dwell_ms..RULE.dwell_ms + 16).contains(&at), "{at} ms");
    // A dwell later it walks on to the seat at the end of the free lead.
    let rest = on(2, 11 + RULE.lead);
    let mut at = 0;
    loop {
        let d = decide(&RULE, &s, state, 16);
        at += 16;
        if d.why != Why::Kept {
            assert_eq!((d.state.seat, d.why), (rest, Why::Moved));
            assert_eq!(path(&RULE, &s, state.seat, d.state.seat), Path::Walk);
            state = d.state;
            break;
        }
        state = d.state;
        assert!(at < 1_000, "it walks on in the end");
    }
    assert!((RULE.dwell_ms..RULE.dwell_ms + 16).contains(&at), "{at} ms");
    // A letter lands under its head: off the glass, owing the rows' quiet.
    let mut inked = g.clone();
    inked.set(1, 30, true);
    inked.set(1, 17, true);
    let y = decide(&RULE, &scene(&inked, (2, 10)), state, 16);
    assert_eq!((y.state.seat, y.why), (Seat::Off(Edge::Left), Why::Yielded));
    assert!(y.state.quiet_owed);
    assert_eq!(
        path(&RULE, &scene(&inked, (2, 10)), state.seat, y.state.seat),
        Path::Cut
    );
    let mut state = y.state;
    for _ in 0..200 {
        let d = decide(&RULE, &s, state, 16);
        assert_eq!((d.state.seat, d.why), (Seat::Off(Edge::Left), Why::Kept));
        state = d.state;
    }
    s.quiet_ms = RULE.return_quiet_ms;
    let d = decide(&RULE, &s, state, 16);
    assert_eq!((d.state.seat, d.why), (out_of_reach, Why::Returned));
    assert!(!d.state.quiet_owed);
    assert_eq!(
        path(&RULE, &s, state.seat, d.state.seat),
        Path::Enter(Edge::Right)
    );
    // A pet that walked off for the writing owes the same quiet: no letter took it off, and a
    // hand still writing would send it off again.
    let mut wall = Grid::blank(6, 40);
    wall.ink.fill(true);
    for c in 36..40 {
        wall.set(2, c, false);
    }
    let mut s = scene(&wall, (2, 36));
    s.ahead = 3;
    let walked = decide(&RULE, &s, sat(small(2, 37)), 16);
    assert_eq!(
        path(&RULE, &s, small(2, 37), walked.state.seat),
        Path::Exit(Edge::Right)
    );
    assert_eq!(
        (walked.state.seat, walked.why),
        (Seat::Off(Edge::Right), Why::GaveWay)
    );
    assert!(walked.state.quiet_owed);
    assert_eq!(walked.state.strikes, 0);
    let mut s = scene(&g, (2, 10));
    s.quiet_ms = RULE.return_quiet_ms - 1;
    let mut state = walked.state;
    for _ in 0..200 {
        let d = decide(&RULE, &s, state, 16);
        assert_eq!((d.state.seat, d.why), (Seat::Off(Edge::Right), Why::Kept));
        state = d.state;
    }
    s.quiet_ms = RULE.return_quiet_ms;
    assert_eq!(decide(&RULE, &s, state, 16).why, Why::Returned);
    // A seat walled in on both sides is no seat to come back to: the nearest one a walk from an
    // edge reaches is, after the dwell, and once in, the pet moves on to the better one after
    // another.
    for r in 0..6 {
        g.set(r, 30, true);
        g.set(r, 0, true);
    }
    let s = scene(&g, (2, 10));
    assert_eq!(best(&RULE, &s).0, rest);
    let mut state = SeatState::offstage(Edge::Right);
    let mut changes = Vec::new();
    let mut at = 0;
    for _ in 0..100 {
        let d = decide(&RULE, &s, state, 16);
        at += 16;
        if d.why != Why::Kept {
            changes.push((d.state.seat, d.why, at));
        }
        state = d.state;
    }
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert_eq!((changes[0].0, changes[0].1), (on(2, 31), Why::Returned));
    assert!((RULE.dwell_ms..RULE.dwell_ms + 16).contains(&changes[0].2));
    assert_eq!((changes[1].0, changes[1].1), (rest, Why::Moved));
    assert!(changes[1].2 - changes[0].2 >= RULE.dwell_ms);
    assert_eq!(
        path(&RULE, &s, Seat::Off(Edge::Right), on(2, 31)),
        Path::Enter(Edge::Right)
    );
    // The way on crosses the lettered column: a leap.
    assert_eq!(path(&RULE, &s, on(2, 31), rest), Path::Leap);
}

/// A shell's page a hand types on, for the typing runs: lines of output over a prompt.
struct Typed {
    g: Grid,
    caret: (u16, u16),
    state: SeatState,
    /// The clock, and when the hand last stepped the caret forward and last changed the rows.
    now: u32,
    stepped: Option<u32>,
    changed: u32,
}

impl Typed {
    fn new(g: Grid, caret: (u16, u16)) -> Self {
        Self {
            g,
            caret,
            state: SeatState::offstage(Edge::Right),
            now: 0,
            stepped: None,
            changed: 0,
        }
    }

    /// A key: a letter at the caret and the caret a cell on, wrapping, scrolling on the last
    /// row.
    fn key(&mut self) {
        let (rows, cols) = (self.g.rows, self.g.cols);
        self.g.set(self.caret.0, self.caret.1, true);
        if self.caret.1 + 1 < cols {
            self.caret.1 += 1;
        } else if self.caret.0 + 1 < rows {
            self.caret = (self.caret.0 + 1, 0);
        } else {
            self.g.ink.drain(0..usize::from(cols));
            self.g
                .ink
                .extend(std::iter::repeat_n(false, usize::from(cols)));
            self.caret.1 = 0;
            self.state = self.state.ride(1, rows, cols);
        }
        self.stepped = Some(self.now);
        self.changed = self.now;
    }

    /// A Backspace over a typed letter, never into the prompt's two cells.
    fn backspace(&mut self) {
        if self.caret.1 > 2 {
            self.caret.1 -= 1;
            self.g.set(self.caret.0, self.caret.1, false);
            self.changed = self.now;
        }
    }

    /// One frame, 16 ms on: the writing is 3 cells ahead for 300 ms after a key, as the replay
    /// stands it in. The decision and the way of its change.
    fn frame(&mut self) -> (Decision, Seat, Path) {
        self.now += 16;
        let mut s = scene(&self.g, self.caret);
        s.below_is_next = true;
        s.ahead = if self.stepped.is_some_and(|at| self.now - at <= 300) {
            3
        } else {
            0
        };
        s.quiet_ms = self.now - self.changed;
        let from = self.state.seat;
        let d = decide(&RULE, &s, self.state, 16);
        assert_clear(&s, d.state.seat, "typing");
        let way = path(&RULE, &s, from, d.state.seat);
        self.state = d.state;
        (d, from, way)
    }
}

#[test]
fn a_typing_hand_never_sends_a_pet_off_the_glass_within_the_dwell_of_its_coming_on() {
    // Keys, Backspaces, wraps, scrolls, breaths and rests, on shells' pages of many sizes; the
    // first is the review's 5 by 48 pane with its prompt on the last row, where the first run
    // of the rule came on the glass for 48 ms.
    let mut rng = Rng(0x5EA7_0004);
    let (mut returns, mut exits, mut keys) = (0u32, 0u32, 0u32);
    for run in 0..300 {
        let (rows, cols, row) = if run == 0 {
            (5, 48, 4)
        } else {
            let (cols, rows) = (20 + rng.u16_below(60), 4 + rng.u16_below(20));
            (rows, cols, rng.u16_below(rows))
        };
        let mut g = Grid::blank(rows, cols);
        for r in 0..row {
            let end = rng.u16_below(cols + 1);
            g.fill_row(r, 0..end);
        }
        g.fill_row(row, 0..2);
        let mut t = Typed::new(g, (row, 2));
        // When it came on the glass, and every change so far: when, from, to.
        let mut on_at = None::<u32>;
        let mut changes: Vec<(u32, Seat, Seat)> = Vec::new();
        for step in 0..400 {
            // A key, a Backspace, a breath or a rest; never two keys within 48 ms.
            let frames = match rng.below(20) {
                0..=11 => {
                    let (before, was_on) = (t.caret, t.state.seat.on_page());
                    t.key();
                    keys += 1;
                    if t.caret.0 == before.0 && t.caret.1 == 0 {
                        // The page scrolled: the seats the changes name rode it, and the pet's
                        // own was not carried off the glass.
                        for c in &mut changes {
                            let rode = |seat: Seat| sat(seat).ride(1, rows, cols).seat;
                            (c.1, c.2) = (rode(c.1), rode(c.2));
                        }
                        assert_eq!(t.state.seat.on_page(), was_on, "run {run}, step {step}");
                    }
                    3 + rng.below(12)
                }
                12..=15 => {
                    t.backspace();
                    3 + rng.below(12)
                }
                16..=18 => 3 + rng.below(28),
                _ => 110 + rng.below(40),
            };
            for _ in 0..frames {
                let (d, from, way) = t.frame();
                if d.why == Why::Kept {
                    continue;
                }
                let (now, to) = (t.now, d.state.seat);
                let what = format!("run {run} ({rows} by {cols}), step {step}, {now} ms");
                // No seat on the page is come back to within the dwell of leaving it, whatever
                // seats lay between.
                for &(then, left, _) in changes.iter().rev() {
                    if now - then >= RULE.dwell_ms {
                        break;
                    }
                    assert!(
                        !(left.on_page() && left.same(to)),
                        "{what}: back on {to:?}, left {} ms ago ({:?})",
                        now - then,
                        d.why
                    );
                }
                match (from.on_page(), to.on_page()) {
                    (false, true) => {
                        assert_eq!(d.why, Why::Returned, "{what}");
                        assert!(matches!(way, Path::Enter(_)), "{what}: {way:?}");
                        on_at = Some(now);
                        returns += 1;
                    }
                    (true, false) => {
                        let came = on_at.take().expect("it came on before it went off");
                        assert!(
                            now - came >= RULE.dwell_ms,
                            "{what}: on the glass at {came} ms and off it {} ms later ({:?} \
                             from {from:?}, caret {:?})",
                            now - came,
                            d.why,
                            t.caret
                        );
                        // No letter lands under a pet the hand alone writes beside: it leaves
                        // on its feet.
                        assert_eq!(d.why, Why::GaveWay, "{what}");
                        exits += 1;
                    }
                    _ => {}
                }
                changes.push((now, from, to));
            }
        }
    }
    assert!(
        returns > 400 && exits > 200 && keys > 50_000,
        "{returns} returns, {exits} exits, {keys} keys"
    );
}

#[test]
fn a_pet_pulled_back_at_a_pause_is_not_sent_out_by_the_next_key() {
    // Type five, pause, Backspace three, pause; three times (a typo and its fix).
    let mut g = Grid::blank(24, 80);
    g.fill_row(10, 0..20);
    let mut t = Typed::new(g, (10, 20));
    for _ in 0..100 {
        t.frame();
    }
    assert_eq!(t.state.seat, on(10, 21 + RULE.lead));
    // Each change: why, the cells the pet went (to the right is more), and how many keys came
    // after the change before it.
    let mut changes: Vec<(Why, i32)> = Vec::new();
    let frames = |t: &mut Typed, n: u32, changes: &mut Vec<(Why, i32)>| {
        for _ in 0..n {
            let (d, from, way) = t.frame();
            if d.why == Why::Kept {
                continue;
            }
            let (Seat::On { col: a, .. }, Seat::On { col: b, .. }) = (from, d.state.seat) else {
                panic!("it stays on the glass: {from:?} to {:?}", d.state.seat);
            };
            assert_eq!(way, Path::Walk, "{from:?} to {:?}", d.state.seat);
            changes.push((d.why, i32::from(b) - i32::from(a)));
        }
    };
    for round in 0..3 {
        for key in 0..5 {
            let before = changes.len();
            t.key();
            frames(&mut t, 6, &mut changes);
            // The first key of a round comes after a rest or a pull back: the pet is out of its
            // way.
            if key == 0 {
                assert_eq!(changes.len(), before, "round {round}: {changes:?}");
            }
        }
        frames(&mut t, 25, &mut changes);
        for _ in 0..3 {
            t.backspace();
            frames(&mut t, 6, &mut changes);
        }
        frames(&mut t, 25, &mut changes);
    }
    // The writing sends it on, a seat or two at a step; a pause pulls it back to the end of the
    // free lead, never under the next keys.
    assert!(
        changes.iter().all(|&(why, by)| match why {
            Why::GaveWay => (1..=2).contains(&by),
            Why::Moved => (-3..0).contains(&by),
            _ => false,
        }),
        "{changes:?}"
    );
    assert_eq!(
        changes.iter().filter(|c| c.0 == Why::Moved).count(),
        3,
        "{changes:?}"
    );
    let Seat::On { col, .. } = t.state.seat else {
        unreachable!("on the glass")
    };
    assert_eq!(col, t.caret.1 + 1 + RULE.lead);
}

/// A page whose rows 9 and 10 a caller letters and clears as its clock says, a pet settled by
/// the caret at (10, 10), run for `ms`: the yields, the returns and the shortest stay on glass.
fn blinked(
    state: &mut SeatState,
    (from, ms): (u32, u32),
    inked: impl Fn(u32) -> Option<(u16, std::ops::Range<u16>)>,
) -> (u32, u32, u32) {
    let mut g = Grid::blank(24, 80);
    g.fill_row(10, 0..10);
    let (mut yields, mut returns, mut shortest) = (0, 0, u32::MAX);
    let mut changed = from;
    // When it came on the glass, for a stay that began in this run.
    let mut on_at = None;
    let mut before = None;
    let mut t = from;
    while t < from + ms {
        t += 16;
        let ink = inked(t);
        if ink != before {
            changed = t;
        }
        before = ink.clone();
        let mut page = g.clone();
        if let Some((row, cols)) = ink {
            page.fill_row(row, cols);
        }
        let mut s = scene(&page, (10, 10));
        s.quiet_ms = t - changed;
        let was_on = state.seat.on_page();
        let d = decide(&RULE, &s, *state, 16);
        assert_clear(&s, d.state.seat, "a page that blinks");
        match d.why {
            Why::Yielded => yields += 1,
            Why::Returned => returns += 1,
            _ => {}
        }
        if d.state.seat.on_page() && !was_on {
            on_at = Some(t);
        }
        if !d.state.seat.on_page()
            && was_on
            && let Some(at) = on_at.take()
        {
            shortest = shortest.min(t - at);
        }
        *state = d.state;
    }
    (yields, returns, shortest)
}

#[test]
fn a_letter_that_comes_and_goes_under_a_seat_is_given_a_wider_berth_each_time() {
    assert_eq!(
        [0u8, 1, 2, 3, 4, 5, 255].map(|strikes| RULE.quiet_owed_ms(strikes)),
        [1_500, 1_500, 3_000, 6_000, 12_000, 12_000, 12_000]
    );
    let settled = || {
        let mut g = Grid::blank(24, 80);
        g.fill_row(10, 0..10);
        let mut state = SeatState::offstage(Edge::Right);
        for _ in 0..100 {
            state = decide(&RULE, &scene(&g, (10, 10)), state, 16).state;
        }
        assert_eq!(state.seat, on(10, 15));
        state
    };
    // A cell under the pet's head lettered for one frame every 2 s, for 30 s: the letter takes
    // it off, it comes back once after 1.5 s of quiet, the letter takes it off again, and the
    // 3 s it then owes never come.
    let mut state = settled();
    let blip = |t: u32| (t % 2_000 < 16).then_some((9, 17..18));
    let (yields, returns, _) = blinked(&mut state, (0, 30_000), blip);
    assert_eq!((yields, returns), (2, 1));
    assert_eq!(state.strikes, 2);
    assert!(!state.seat.on_page());
    // The letter stops coming: 3 s later the pet is back, and after 30 s on the glass with no
    // letter under it the two are forgiven.
    let (yields, returns, _) = blinked(&mut state, (30_000, 3_400), |_| None);
    assert_eq!((yields, returns), (0, 1));
    assert_eq!(state.strikes, 2);
    blinked(&mut state, (33_400, RULE.forgive_ms), |_| None);
    assert_eq!((state.strikes, state.seat), (0, on(10, 15)));
    // Text on the row over the caret's for 300 ms every 3 s (a notice that comes and goes):
    // the same, where the first run of the rule went and came every period for ever.
    let mut state = settled();
    let notice = |t: u32| (t % 3_000 < 300).then_some((9, 0..40));
    let (yields, returns, shortest) = blinked(&mut state, (0, 30_000), notice);
    assert_eq!((yields, returns), (2, 1));
    assert!(shortest >= RULE.dwell_ms, "{shortest} ms");
    // One every 20 s: the pet comes back after each, 1.5 s later at first and 12 s at most,
    // and is on the glass 8 s or more each time.
    let mut state = settled();
    let rare = |t: u32| (t % 20_000 < 16).then_some((9, 17..18));
    let (yields, returns, shortest) = blinked(&mut state, (0, 120_000), rare);
    assert_eq!((yields, returns), (6, 5), "{state:?}");
    assert!(shortest >= 5_000, "{shortest} ms");
}

#[test]
fn letters_the_hand_lands_along_its_line_are_writing_and_no_letter_under_the_pet() {
    let mut g = Grid::blank(24, 80);
    g.fill_row(10, 0..10);
    let mut rest = SeatState::offstage(Edge::Right);
    for _ in 0..100 {
        rest = decide(&RULE, &scene(&g, (10, 10)), rest, 16).state;
    }
    assert_eq!((rest.seat, rest.caret), (on(10, 15), Some((10, 10))));
    // A paste, a completion, an input method's commit: `n` cells in one frame at rest, with no
    // tempo to see them coming. The pet stays, walks on or leaps on; it is never cut.
    for n in 1..=60u16 {
        let mut page = g.clone();
        page.fill_row(10, 10..10 + n);
        let mut s = scene(&page, (10, 10 + n));
        s.quiet_ms = 0;
        let d = decide(&RULE, &s, rest, 16);
        assert_clear(&s, d.state.seat, "a paste");
        let way = path(&RULE, &s, rest.seat, d.state.seat);
        match n {
            // One wide glyph and more: short of the seat and its gap.
            1..=3 => assert_eq!((d.why, way), (Why::Kept, Path::Stay), "{n} cells"),
            4 => assert_eq!((d.why, way), (Why::GaveWay, Path::Walk), "{n} cells"),
            _ => assert_eq!((d.why, way), (Why::GaveWay, Path::Leap), "{n} cells"),
        }
        assert!(d.state.seat.on_page(), "{n} cells: {:?}", d.state.seat);
        assert!(!d.state.quiet_owed && d.state.strikes == 0, "{n} cells");
    }
    // A shell's suggestion: words past a caret that did not move, with their spaces. The pet
    // goes on past its end, to the far end of the free lead there.
    let mut page = g.clone();
    page.fill_row(10, 10..15);
    page.fill_row(10, 16..23);
    let s = scene(&page, (10, 10));
    let d = decide(&RULE, &s, rest, 16);
    assert_eq!((d.state.seat, d.why), (on(10, 27), Why::GaveWay));
    assert_eq!(path(&RULE, &s, rest.seat, d.state.seat), Path::Leap);
    // A paste that wraps under a pet on the next row.
    let below = SeatState {
        seat: small(11, 2),
        caret: Some((10, 75)),
        ..rest
    };
    let mut page = Grid::blank(24, 80);
    page.fill_row(10, 0..80);
    page.fill_row(11, 0..5);
    let s = scene(&page, (11, 5));
    let d = decide(&RULE, &s, below, 16);
    assert_eq!(d.why, Why::GaveWay);
    assert!(d.state.seat.on_page());
    // A letter over the line (under the pet's head) is no writing of the hand's: the pet is off
    // the glass that frame, as for any program's output.
    let mut page = g.clone();
    page.fill_row(10, 10..18);
    page.set(9, 17, true);
    let d = decide(&RULE, &scene(&page, (10, 18)), rest, 16);
    assert_eq!((d.state.seat, d.why), (Seat::Off(Edge::Left), Why::Yielded));
    // Nor are letters between two carets a program moved far apart.
    let far = SeatState {
        caret: Some((2, 0)),
        ..rest
    };
    let mut page = g.clone();
    page.set(10, 17, true);
    let d = decide(&RULE, &scene(&page, (20, 0)), far, 16);
    assert_eq!(d.why, Why::Yielded);
    // Nor letters behind where the caret was on its row.
    let behind = SeatState {
        seat: on(10, 2),
        caret: Some((10, 10)),
        ..rest
    };
    let mut page = Grid::blank(24, 80);
    page.set(10, 4, true);
    let d = decide(&RULE, &scene(&page, (10, 12)), behind, 16);
    assert_eq!(d.why, Why::Yielded);
}

#[test]
fn on_another_page_the_pet_keeps_its_seat_or_goes_to_the_new_page_s_best() {
    let mut g = Grid::blank(24, 80);
    g.fill_row(10, 0..10);
    let mut rest = SeatState::offstage(Edge::Right);
    for _ in 0..100 {
        rest = decide(&RULE, &scene(&g, (10, 10)), rest, 16).state;
    }
    assert_eq!(rest.seat, on(10, 15));
    // The other screen: text on every row but the last, the caret at the last row's start.
    let mut other = Grid::blank(24, 80);
    for r in 0..23 {
        other.fill_row(r, 0..78);
    }
    let mut s = scene(&other, (23, 0));
    s.quiet_ms = 0;
    // Said to be the same page, the letters landed under the pet: a yield, a cut, quiet owed.
    let d = decide(&RULE, &s, rest, 16);
    assert_eq!((d.why, d.state.quiet_owed), (Why::Yielded, true));
    // Said to be another page, no letter landed: the pet is on the new page's best seat that
    // frame, by a leap, owing nothing, with no strike against it.
    s.new_page = true;
    let d = decide(&RULE, &s, rest, 16);
    assert_eq!((d.state.seat, d.why), (small(23, 5), Why::Switched));
    assert_eq!(path(&RULE, &s, rest.seat, d.state.seat), Path::Leap);
    assert!(!d.state.quiet_owed && d.state.strikes == 0);
    assert_eq!(d.state.barred, NO_BARS);
    // A seat that is one on the new page too is kept, and left for a better one only after the
    // dwell (a caret the switch showed for one frame moves nothing).
    let mut same = scene(&g, (20, 40));
    same.new_page = true;
    let d = decide(&RULE, &same, rest, 16);
    assert_eq!((d.state.seat, d.why), (rest.seat, Why::Kept));
    // A page with no seat: off the glass, not drawn on the way; and back on its feet once the
    // first page is back and its rows have been quiet.
    let mut full = Grid::blank(24, 80);
    full.ink.fill(true);
    let mut s = scene(&full, (23, 0));
    s.new_page = true;
    let d = decide(&RULE, &s, rest, 16);
    assert_eq!(
        (d.state.seat, d.why),
        (Seat::Off(Edge::Left), Why::Switched)
    );
    assert_eq!(path(&RULE, &s, rest.seat, d.state.seat), Path::Cut);
    assert!(d.state.quiet_owed && d.state.strikes == 0);
    let mut state = d.state;
    let mut back = None;
    for frame in 0..200u32 {
        let mut s = scene(&g, (10, 10));
        s.new_page = frame == 0;
        s.quiet_ms = frame * 16;
        let d = decide(&RULE, &s, state, 16);
        if d.why != Why::Kept {
            assert_eq!(d.why, Why::Returned);
            assert!(matches!(
                path(&RULE, &s, state.seat, d.state.seat),
                Path::Enter(_)
            ));
            back = Some(s.quiet_ms);
            state = d.state;
            break;
        }
        state = d.state;
    }
    assert!(
        back.is_some_and(|at| (RULE.return_quiet_ms..RULE.return_quiet_ms + 16).contains(&at)),
        "{back:?}"
    );
    assert!(state.seat.on_page());
}

#[test]
fn the_quiet_a_return_waits_for_is_the_quiet_of_the_rows_the_rule_reads() {
    assert_eq!(RULE.reads(PET, 10, 24), (8, 13));
    assert_eq!(RULE.reads(PET, 0, 24), (0, 3));
    assert_eq!(RULE.reads(PET, 23, 24), (21, 23));
    assert_eq!(Rule::FULL_SCREEN.reads(PET, 10, 24), (8, 10));
    assert_eq!(RULE.reads(PET, 40, 24), (23, 23));
    assert_eq!(RULE.reads(PET, 0, 0), (0, 0));
    // No seat the search names, and no cell a way in passes over, is outside those rows.
    let mut rng = Rng(0x5EA7_0006);
    for case in 0..2_000 {
        let g = generated_page(&mut rng);
        let body = generated_body(&mut rng);
        let rule = generated_rule(&mut rng);
        let caret = (rng.u16_below(g.rows), rng.u16_below(g.cols));
        let s = Scene {
            body,
            ahead: rng.u16_below(12),
            ..scene(&g, caret)
        };
        let (first, last) = rule.reads(body, caret.0, g.rows);
        if let Seat::On { row, small, .. } = best(&rule, &s).0 {
            let tall = if small { 1 } else { body.whole.rows };
            assert!(
                row <= last && row + 1 >= first + tall.min(row + 1),
                "case {case}: feet on {row}, {tall} tall, reads {first}..={last}"
            );
        }
    }
    // A pet a letter took off the glass, and a spinner that turns every 100 ms on the page's
    // first row: the rows about the caret are quiet, and it is back after 1.5 s of that.
    let g = Grid::blank(24, 80);
    let (first, _) = RULE.reads(PET, 10, 24);
    assert!(first > 0);
    let mut state = SeatState {
        seat: Seat::Off(Edge::Left),
        quiet_owed: true,
        strikes: 1,
        ..SeatState::offstage(Edge::Left)
    };
    let mut back = None;
    for frame in 1..=200u32 {
        let mut page = g.clone();
        page.set(0, 79, frame * 16 / 100 % 2 == 0);
        let mut s = scene(&page, (10, 10));
        s.quiet_ms = frame * 16;
        let d = decide(&RULE, &s, state, 16);
        state = d.state;
        if d.why == Why::Returned {
            back = Some(frame * 16);
            break;
        }
    }
    assert_eq!(back, Some(1_504));
}

#[test]
fn a_seat_far_from_the_caret_on_the_widest_pane_is_judged_without_overflow() {
    let page = Blank(u16::MAX, u16::MAX);
    let s = Scene {
        page: &page,
        caret: (0, 0),
        body: Body {
            whole: Size { cols: 1, rows: 1 },
            small: None,
        },
        ahead: u16::MAX,
        below_is_next: true,
        quiet_ms: 0,
        hold: false,
        new_page: false,
        scrolled: false,
    };
    let far = Seat::On {
        row: u16::MAX - 1,
        col: u16::MAX - 1,
        small: false,
    };
    assert!(legal(&RULE, &s, far));
    assert!(!legal(&RULE, &s, on(1, 0)), "the writing wraps onto it");
    let state = SeatState {
        seat: far,
        ..SeatState::offstage(Edge::Right)
    };
    assert_eq!(decide(&RULE, &s, state, 16).why, Why::Kept);
}

#[test]
fn the_way_between_two_seats_is_a_walk_a_leap_or_an_edge() {
    let mut g = Grid::blank(6, 40);
    let s = scene(&g, (2, 10));
    assert_eq!(path(&RULE, &s, on(2, 12), on(2, 12)), Path::Stay);
    assert_eq!(path(&RULE, &s, on(2, 12), on(2, 20)), Path::Walk);
    assert_eq!(path(&RULE, &s, on(2, 20), on(2, 12)), Path::Walk);
    // Across the caret's cell, to another row, or to the other drawing: a leap.
    assert_eq!(path(&RULE, &s, on(2, 12), on(2, 2)), Path::Leap);
    assert_eq!(path(&RULE, &s, on(2, 12), on(3, 12)), Path::Leap);
    assert_eq!(path(&RULE, &s, on(2, 12), small(2, 12)), Path::Leap);
    // Off through the edge named where the way is clear, else the other, else a cut.
    assert_eq!(
        path(&RULE, &s, on(2, 12), Seat::Off(Edge::Right)),
        Path::Exit(Edge::Right)
    );
    assert_eq!(
        path(&RULE, &s, on(2, 12), Seat::Off(Edge::Left)),
        Path::Exit(Edge::Right),
        "the caret's cell is in the way to the left edge"
    );
    assert_eq!(
        path(&RULE, &s, on(4, 12), Seat::Off(Edge::Left)),
        Path::Exit(Edge::Left)
    );
    g.set(1, 30, true);
    let s = scene(&g, (2, 10));
    assert_eq!(
        path(&RULE, &s, on(2, 12), Seat::Off(Edge::Right)),
        Path::Cut
    );
    assert_eq!(
        path(&RULE, &s, Seat::Off(Edge::Right), on(2, 12)),
        Path::Cut
    );
    assert_eq!(
        path(&RULE, &s, Seat::Off(Edge::Left), on(2, 31)),
        Path::Enter(Edge::Right)
    );
    assert_eq!(
        path(&RULE, &s, Seat::Off(Edge::Left), Seat::Off(Edge::Right)),
        Path::Stay
    );
    // A letter under the seat it leaves: no way on its feet.
    g.set(2, 14, true);
    let s = scene(&g, (2, 10));
    assert_eq!(path(&RULE, &s, on(2, 12), on(2, 20)), Path::Leap);
    // The writing's next cells hold nothing yet: no obstacle to a way.
    let g = Grid::blank(6, 40);
    let mut s = scene(&g, (2, 10));
    s.ahead = 4;
    assert!(!legal(&RULE, &s, on(2, 12)));
    assert_eq!(
        path(&RULE, &s, on(2, 12), Seat::Off(Edge::Right)),
        Path::Exit(Edge::Right)
    );
}

#[test]
fn a_seat_rides_the_page_and_goes_off_the_glass_past_its_rows() {
    let state = sat(small(5, 30));
    assert_eq!(state.ride(2, 14, 72).seat, small(3, 30));
    assert_eq!(state.ride(-2, 14, 72).seat, small(7, 30));
    assert_eq!(state.ride(6, 14, 72).seat, Seat::Off(Edge::Left));
    assert_eq!(sat(on(5, 40)).ride(-9, 14, 72).seat, Seat::Off(Edge::Right));
    let off = SeatState::offstage(Edge::Left);
    assert_eq!(off.ride(3, 14, 72), off);
    // A seat the page carried off the glass owes the rows' quiet; one it carried along does not.
    assert!(state.ride(6, 14, 72).quiet_owed);
    assert!(!state.ride(2, 14, 72).quiet_owed);
    // The seats it left and the caret it remembers are places on the page: they ride it too,
    // and are forgotten where it carried them off.
    let bar = |row: u16| Bar {
        row,
        small: false,
        first: 30,
        last: 33,
        ago_ms: 16,
    };
    let state = SeatState {
        seat: on(20, 40),
        caret: Some((20, 28)),
        barred: [Some(bar(20)), None, Some(bar(0)), None],
        ..SeatState::offstage(Edge::Right)
    };
    let rode = state.ride(1, 24, 80);
    assert_eq!((rode.seat, rode.caret), (on(19, 40), Some((19, 28))));
    assert_eq!(rode.barred, [Some(bar(19)), None, None, None]);
    assert!(bars(&rode, on(19, 30)) && !bars(&rode, on(20, 30)));
    assert_eq!(state.ride(21, 24, 80).caret, None);
    // The seat it gave way from a frame before the page scrolled is barred where it is now.
    let g = Grid::blank(24, 80);
    let mut s = scene(&g, (23, 41));
    s.ahead = 3;
    let d = decide(&RULE, &s, sat(on(23, 40)), 16);
    assert!(d.state.seat.on_page() && bars(&d.state, on(23, 40)));
    assert!(bars(&d.state.ride(1, 24, 80), on(22, 40)));
}

/// xorshift64*: the generated pages' only source, so a failure names its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn u16_below(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(u64::from(n))).unwrap()
    }
}

fn generated_page(rng: &mut Rng) -> Grid {
    let rows = 1 + rng.u16_below(18);
    let cols = 1 + rng.u16_below(90);
    let mut g = Grid::blank(rows, cols);
    // Blank, sparse, lines of words, dense, full.
    match rng.below(5) {
        0 => {}
        1 => {
            for cell in &mut g.ink {
                *cell = rng.below(20) == 0;
            }
        }
        2 => {
            for r in 0..rows {
                let end = rng.u16_below(cols + 1);
                for c in 0..end {
                    g.set(r, c, rng.below(6) != 0);
                }
            }
        }
        3 => {
            for cell in &mut g.ink {
                *cell = rng.below(3) != 0;
            }
        }
        _ => g.ink.fill(true),
    }
    // One page in four has a rule or two drawn across it.
    if rng.below(4) == 0 {
        for _ in 0..1 + rng.below(2) {
            let row = rng.u16_below(rows);
            if !g.fences.contains(&row) {
                g.rule(row);
            }
        }
    }
    g
}

fn generated_body(rng: &mut Rng) -> Body {
    let whole = Size {
        cols: 1 + rng.u16_below(7),
        rows: 1 + rng.u16_below(3),
    };
    let small = (rng.below(3) != 0).then(|| Size {
        cols: 1 + rng.u16_below(whole.cols),
        rows: 1,
    });
    Body { whole, small }
}

fn generated_seat(rng: &mut Rng, g: &Grid, body: Body) -> Seat {
    match rng.below(5) {
        0 => Seat::Off(if rng.below(2) == 0 {
            Edge::Left
        } else {
            Edge::Right
        }),
        // Anywhere, past the pane too.
        1 => Seat::On {
            row: rng.u16_below(g.rows + 3),
            col: rng.u16_below(g.cols + 3),
            small: rng.below(2) == 0,
        },
        _ => Seat::On {
            row: rng.u16_below(g.rows),
            col: rng.u16_below(g.cols),
            small: body.small.is_some() && rng.below(2) == 0,
        },
    }
}

fn generated_rule(rng: &mut Rng) -> Rule {
    if rng.below(4) == 0 {
        Rule::FULL_SCREEN
    } else {
        Rule::DEFAULT
    }
}

/// Whether a rule is drawn between the caret's row and `row`, or on `row`.
fn across_a_rule(s: &Scene<'_, Grid>, row: u16) -> bool {
    let cr = s.caret.0;
    s.page
        .fences
        .iter()
        .any(|&f| f != cr && f >= row.min(cr) && f <= row.max(cr))
}

/// The best cost read the slow way: every seat on the rule's rows asked of [`legal`] and
/// [`cost`] one by one, and the seat off the glass. A row with a rule drawn between it and the
/// caret's, or on it, has no seat.
fn slow_best_cost(rule: &Rule, s: &Scene<'_, Grid>) -> u32 {
    let mut least = cost(rule, s, Seat::Off(Edge::Left));
    let top = s.caret.0.saturating_sub(rule.rows_up);
    let bottom = (s.caret.0 + rule.rows_down).min(s.page.rows - 1);
    for row in top..=bottom {
        if across_a_rule(s, row) {
            continue;
        }
        for col in 0..s.page.cols {
            for small in [false, true] {
                let seat = Seat::On { row, col, small };
                if legal(rule, s, seat) {
                    least = least.min(cost(rule, s, seat));
                }
            }
        }
    }
    least
}

fn generated_bars(rng: &mut Rng, g: &Grid) -> [Option<Bar>; BARS] {
    std::array::from_fn(|_| {
        (rng.below(3) == 0).then(|| {
            let first = rng.u16_below(g.cols);
            Bar {
                row: rng.u16_below(g.rows),
                small: rng.below(2) == 0,
                first,
                last: first + rng.u16_below(4),
                ago_ms: u32::try_from(rng.below(400)).unwrap(),
            }
        })
    })
}

#[test]
fn on_generated_pages_every_decision_is_a_clear_seat_and_the_same_one_twice() {
    let mut rng = Rng(0x5EA7_0001);
    let (mut on_page, mut off, mut yielded, mut gave_way, mut switched, mut moved, mut returned) =
        (0, 0, 0, 0, 0, 0, 0);
    for case in 0..20_000 {
        let g = generated_page(&mut rng);
        let body = generated_body(&mut rng);
        let rule = generated_rule(&mut rng);
        let s = Scene {
            page: &g,
            caret: (rng.u16_below(g.rows), rng.u16_below(g.cols)),
            body,
            ahead: rng.u16_below(7),
            below_is_next: rng.below(2) == 0,
            quiet_ms: [0, 2_000, 20_000][usize::try_from(rng.below(3)).unwrap()],
            hold: rng.below(4) == 0,
            new_page: rng.below(8) == 0,
            scrolled: rng.below(8) == 0,
        };
        let state = SeatState {
            seat: generated_seat(&mut rng, &g, body),
            caret: (rng.below(2) == 0).then(|| (rng.u16_below(g.rows), rng.u16_below(g.cols))),
            barred: generated_bars(&mut rng, &g),
            beaten_ms: u32::try_from(rng.below(400)).unwrap(),
            quiet_owed: rng.below(2) == 0,
            strikes: u8::try_from(rng.below(6)).unwrap(),
            calm_ms: u32::try_from(rng.below(40_000)).unwrap(),
        };
        let dt = 1 + u32::try_from(rng.below(100)).unwrap();
        let what = format!("case {case}");

        // The one rule: a clear seat, the cheapest there is, the same asked twice.
        let (seat, seat_cost) = best(&rule, &s);
        assert_clear(&s, seat, &what);
        assert!(legal(&rule, &s, seat), "{what}: {seat:?}");
        assert_eq!(cost(&rule, &s, seat), seat_cost, "{what}: {seat:?}");
        assert_eq!(seat_cost, slow_best_cost(&rule, &s), "{what}: {seat:?}");
        assert_eq!(best(&rule, &s), (seat, seat_cost), "{what}");

        // The decision: always a seat, a clear one, the same asked twice.
        let d = decide(&rule, &s, state, dt);
        assert_eq!(decide(&rule, &s, state, dt), d, "{what}");
        assert_clear(&s, d.state.seat, &what);
        assert!(legal(&rule, &s, d.state.seat), "{what}: {d:?}");
        assert_eq!(d.state.caret, Some(s.caret), "{what}");
        // The strikes still held against it, and the seats still barred, read without the rule.
        let on_glass = state.seat.on_page();
        let calm = state.calm_ms + if on_glass { dt } else { 0 };
        let strikes = if calm >= rule.forgive_ms {
            0
        } else {
            state.strikes
        };
        let barred_still = |seat: Seat| {
            !s.new_page
                && state
                    .barred
                    .iter()
                    .flatten()
                    .any(|b| b.ago_ms + dt < rule.dwell_ms && b.holds(seat))
        };
        if d.why != Why::Kept {
            assert!(
                !d.state.seat.same(state.seat),
                "{what}: a change changes it"
            );
            assert!(
                !barred_still(d.state.seat),
                "{what}: {:?} to a seat left within the dwell: {:?}",
                d.why,
                d.state.seat
            );
            assert_eq!(d.state.quiet_owed, !d.state.seat.on_page(), "{what}");
            assert_eq!(d.state.beaten_ms, 0, "{what}");
        }
        if d.why != Why::Yielded {
            assert_eq!(d.state.strikes, strikes, "{what}");
        }
        match d.why {
            Why::Kept => {
                assert!(d.state.seat == state.seat, "{what}: kept is the same seat");
                assert_eq!(d.state.quiet_owed, state.quiet_owed, "{what}");
            }
            Why::Yielded => {
                // A letter under it, or the pane's edge through it: off the glass, one more
                // strike against it.
                assert!(on_glass && !d.state.seat.on_page() && !s.new_page, "{what}");
                let out_of_the_pane = match state.seat {
                    Seat::On { row, col, small } => {
                        let size = if small { body.small } else { Some(body.whole) };
                        row >= g.rows || size.is_none_or(|size| col + size.cols > g.cols)
                    }
                    Seat::Off(_) => false,
                };
                assert!(
                    !s.scrolled || out_of_the_pane,
                    "{what}: yielded to letters a scroll brought"
                );
                assert!(
                    on_letters_or_out(&s, state.seat),
                    "{what}: yielded a seat clear of letters"
                );
                assert_eq!(d.state.strikes, strikes + 1, "{what}");
                assert_eq!(d.state.calm_ms, 0, "{what}");
                yielded += 1;
            }
            Why::GaveWay => {
                // The caret or the writing in it, and no letter but the hand's own line.
                assert!(
                    !legal(&rule, &s, state.seat) && !s.new_page,
                    "{what}: gave way from a seat"
                );
                for (r, c) in box_cells(body, g.rows, state.seat) {
                    let (r16, c16) = (u16::try_from(r).unwrap(), u16::try_from(c).unwrap());
                    assert!(c16 < g.cols, "{what}: gave way where the pane cut it");
                    assert!(
                        s.scrolled
                            || !g.lettered(r16, c16)
                            || r16 <= s.caret.0 && (r16 == s.caret.0 || state.caret.is_some()),
                        "{what}: gave way where a letter lay under it at ({r}, {c})"
                    );
                }
                gave_way += 1;
            }
            Why::Switched => {
                assert!(s.new_page && !legal(&rule, &s, state.seat), "{what}");
                assert_eq!(d.state.seat, best(&rule, &s).0, "{what}");
                assert_eq!(d.state.barred, NO_BARS, "{what}");
                switched += 1;
            }
            Why::Moved => {
                assert!(on_glass && !s.hold, "{what}");
                assert!(
                    cost(&rule, &s, d.state.seat) + rule.margin <= cost(&rule, &s, state.seat),
                    "{what}: moved for less than the margin"
                );
                assert!(state.beaten_ms + dt >= rule.dwell_ms, "{what}");
                moved += 1;
            }
            Why::Returned => {
                assert!(!on_glass && d.state.seat.on_page() && !s.hold, "{what}");
                assert!(state.beaten_ms + dt >= rule.dwell_ms, "{what}");
                assert!(
                    !state.quiet_owed || s.quiet_ms >= rule.quiet_owed_ms(strikes),
                    "{what}: back before the quiet it owes"
                );
                assert!(
                    matches!(path(&rule, &s, state.seat, d.state.seat), Path::Enter(_)),
                    "{what}: it comes back on its feet"
                );
                let reached = Scene {
                    ahead: s.ahead.max(rule.reach),
                    ..s
                };
                assert!(
                    legal(&rule, &reached, d.state.seat),
                    "{what}: back inside the hand's reach"
                );
                returned += 1;
            }
        }
        if d.state.seat.on_page() {
            on_page += 1;
        } else {
            off += 1;
        }
    }
    // The generator reaches every answer.
    assert!(
        on_page > 2_000
            && off > 2_000
            && yielded > 2_000
            && gave_way > 200
            && switched > 200
            && moved > 50
            && returned > 50,
        "on {on_page}, off {off}, yielded {yielded}, gave way {gave_way}, switched {switched}, \
         moved {moved}, returned {returned}"
    );
}

#[test]
fn on_a_page_that_holds_still_the_seat_settles() {
    let mut rng = Rng(0x5EA7_0002);
    let mut most = 0;
    for case in 0..3_000 {
        let g = generated_page(&mut rng);
        let body = generated_body(&mut rng);
        let rule = generated_rule(&mut rng);
        let s = Scene {
            page: &g,
            caret: (rng.u16_below(g.rows), rng.u16_below(g.cols)),
            body,
            ahead: rng.u16_below(4),
            below_is_next: rng.below(2) == 0,
            quiet_ms: 5_000,
            hold: false,
            new_page: false,
            scrolled: false,
        };
        let mut state = SeatState {
            seat: generated_seat(&mut rng, &g, body),
            ..SeatState::offstage(Edge::Right)
        };
        let mut changes = 0;
        let mut last_change = 0;
        for frame in 0..120 {
            let d = decide(&rule, &s, state, 16);
            if d.why != Why::Kept {
                changes += 1;
                last_change = frame;
            }
            state = d.state;
        }
        // A seat that is none is left; a pet off the glass comes in to the best seat a walk
        // reaches out of the hand's reach; then one move to the best seat there is. No more,
        // and all within three dwells.
        assert!(changes <= 3, "case {case}: {changes} changes");
        assert!(
            last_change * 16 <= 3 * rule.dwell_ms + 3 * 16,
            "case {case}: a change at frame {last_change}"
        );
        // Where it settled, nothing is better by the margin.
        let (_, least) = best(&rule, &s);
        assert!(
            cost(&rule, &s, state.seat) < least + rule.margin
                || !state.seat.on_page() && !best_walk_in(&rule, &s),
            "case {case}: settled on {:?} at {} with {least} to be had",
            state.seat,
            cost(&rule, &s, state.seat)
        );
        most = most.max(changes);
    }
    assert!(most >= 2, "the generator reaches a second change");
}

/// Whether some seat on the page a walk from an edge reaches, out of the hand's reach, is
/// better than off the glass by the margin.
fn best_walk_in(rule: &Rule, s: &Scene<'_, Grid>) -> bool {
    let off = cost(rule, s, Seat::Off(Edge::Left));
    let reached = Scene {
        ahead: s.ahead.max(rule.reach),
        ..*s
    };
    let coming = Rule {
        lead: rule.lead.max(rule.reach),
        ..*rule
    };
    let top = s.caret.0.saturating_sub(rule.rows_up);
    let bottom = (s.caret.0 + rule.rows_down).min(s.page.rows - 1);
    (top..=bottom).any(|row| {
        !across_a_rule(s, row)
            && (0..s.page.cols).any(|col| {
                [false, true].into_iter().any(|small| {
                    let seat = Seat::On { row, col, small };
                    legal(rule, &reached, seat)
                        && cost(&coming, s, seat) + rule.margin <= off
                        && path_with_claims(rule, &reached, seat).is_some()
                })
            })
    })
}

/// The edge a walk in to `seat` comes from, the writing's next cells counted as in the way (as
/// [`decide`] counts them for a return).
fn path_with_claims(rule: &Rule, s: &Scene<'_, Grid>, seat: Seat) -> Option<Edge> {
    let Seat::On { row, col, small } = seat else {
        return None;
    };
    let size = if small { s.body.small? } else { s.body.whole };
    let clear = |cols: std::ops::Range<u16>| {
        cols.into_iter().all(|c| {
            (0..size.rows).all(|k| {
                row < k || {
                    let cell = Seat::On {
                        row: row - k,
                        col: c,
                        small: true,
                    };
                    let one = Scene {
                        body: Body {
                            whole: Size { cols: 1, rows: 1 },
                            small: Some(Size { cols: 1, rows: 1 }),
                        },
                        ..*s
                    };
                    legal(rule, &one, cell)
                }
            })
        })
    };
    if clear(0..col + size.cols) {
        Some(Edge::Left)
    } else if clear(col..s.page.cols) {
        Some(Edge::Right)
    } else {
        None
    }
}

#[test]
fn over_a_page_that_changes_the_pet_never_goes_back_to_a_seat_within_the_dwell() {
    let mut rng = Rng(0x5EA7_0003);
    let (mut total_changes, mut returns, mut forced_back_off, mut switches) =
        (0u32, 0u32, 0u32, 0u32);
    for run in 0..400 {
        let mut g = generated_page(&mut rng);
        let body = generated_body(&mut rng);
        let rule = generated_rule(&mut rng);
        let mut caret = (rng.u16_below(g.rows), rng.u16_below(g.cols));
        let mut state = SeatState::offstage(Edge::Right);
        let mut quiet_ms = 5_000u32;
        let mut now = 0u32;
        let mut been_on = false;
        // The changes on this page so far: when, from, to.
        let mut changes: Vec<(u32, Seat, Seat)> = Vec::new();
        for step in 0..600 {
            let dt = 4 + u32::try_from(rng.below(40)).unwrap();
            now += dt;
            quiet_ms = quiet_ms.saturating_add(dt);
            let mut ahead = 0;
            let mut new_page = false;
            // Busy for a hundred steps, then still for a hundred.
            let busy = (step / 100) % 2 == 0;
            match if busy { rng.below(14) } else { 13 } {
                // A key: a letter at the caret, the caret a cell on (wrapping).
                0..=3 => {
                    g.set(caret.0, caret.1, true);
                    caret = if caret.1 + 1 < g.cols {
                        (caret.0, caret.1 + 1)
                    } else {
                        ((caret.0 + 1) % g.rows, 0)
                    };
                    ahead = 3;
                    quiet_ms = 0;
                }
                // A Backspace.
                4 => {
                    if caret.1 > 0 {
                        caret.1 -= 1;
                        g.set(caret.0, caret.1, false);
                        quiet_ms = 0;
                    }
                }
                // Output: a few cells flip anywhere.
                5 => {
                    for _ in 0..1 + rng.below(4) {
                        let (r, c) = (rng.u16_below(g.rows), rng.u16_below(g.cols));
                        let on = rng.below(2) == 0;
                        g.set(r, c, on);
                    }
                    quiet_ms = 0;
                }
                // The caret jumps.
                6 => caret = (rng.u16_below(g.rows), rng.u16_below(g.cols)),
                // A row clears.
                7 if rng.below(4) == 0 => {
                    let r = rng.u16_below(g.rows);
                    for c in 0..g.cols {
                        g.set(r, c, false);
                    }
                    quiet_ms = 0;
                }
                // A paste along the caret's row.
                8 => {
                    let n = 1 + rng.u16_below(12);
                    for _ in 0..n {
                        if caret.1 + 1 < g.cols {
                            g.set(caret.0, caret.1, true);
                            caret.1 += 1;
                        }
                    }
                    quiet_ms = 0;
                }
                // The page scrolls a row up under the pet.
                9 if g.rows > 1 => {
                    g.ink.drain(0..usize::from(g.cols));
                    g.ink
                        .extend(std::iter::repeat_n(false, usize::from(g.cols)));
                    state = state.ride(1, g.rows, g.cols);
                    for c in &mut changes {
                        let rode = |seat: Seat| sat(seat).ride(1, g.rows, g.cols).seat;
                        (c.1, c.2) = (rode(c.1), rode(c.2));
                    }
                    quiet_ms = 0;
                }
                // Another page.
                10 if rng.below(6) == 0 => {
                    let (rows, cols) = (g.rows, g.cols);
                    g = generated_page(&mut rng);
                    while (g.rows, g.cols) != (rows, cols) {
                        g = generated_page(&mut rng);
                    }
                    new_page = true;
                    changes.clear();
                    quiet_ms = 0;
                }
                _ => {}
            }
            let s = Scene {
                page: &g,
                caret,
                body,
                ahead,
                below_is_next: run % 2 == 0,
                quiet_ms,
                hold: rng.below(10) == 0,
                new_page,
                scrolled: false,
            };
            let from = state.seat;
            let d = decide(&rule, &s, state, dt);
            let what = format!("run {run}, step {step}");
            assert_clear(&s, d.state.seat, &what);
            state = d.state;
            if d.why == Why::Kept {
                continue;
            }
            let to = d.state.seat;
            assert!(!to.same(from), "{what}: a change changes the seat");
            // Every change within the dwell, not the last alone: A, B, C, A is a blink too.
            for (k, &(then, left, _)) in changes.iter().rev().enumerate() {
                if now - then >= rule.dwell_ms {
                    break;
                }
                if !to.same(left) {
                    continue;
                }
                // Back within the dwell: only off the glass, and only because the seat it had
                // stopped being one.
                assert!(
                    !left.on_page() && matches!(d.why, Why::Yielded | Why::GaveWay | Why::Switched),
                    "{what}: left {left:?} and back {} ms and {} changes later ({:?})",
                    now - then,
                    k + 1,
                    d.why
                );
                forced_back_off += 1;
                break;
            }
            match d.why {
                Why::Returned => {
                    // On the glass before: it left it, and comes back to quiet rows only.
                    assert!(
                        !been_on || quiet_ms >= rule.return_quiet_ms,
                        "{what}: back on a busy page"
                    );
                    returns += 1;
                }
                Why::Switched => {
                    assert!(new_page, "{what}");
                    switches += 1;
                }
                _ => {}
            }
            been_on |= to.on_page();
            changes.push((now, from, to));
            total_changes += 1;
        }
    }
    assert!(
        total_changes > 5_000 && returns > 100 && switches > 20,
        "{total_changes} changes, {returns} returns, {switches} switches, {forced_back_off} \
         forced back off the glass"
    );
}

/// The trail film's `coloured-prompt` page: a shell's full page of 72 columns, the prompt on its
/// last row.
fn prompt_page() -> Grid {
    let mut g = Grid::blank(14, 72);
    for r in 0..13 {
        g.write(
            r,
            "test result: ok. 3 passed; 0 failed; 0 ignored; finished in 0.42s",
        );
    }
    g.write(13, "$");
    g
}

const SUGGESTION: &str = "$ git commit -m 'quick brown fox jumps'";

#[test]
fn the_hand_s_line_ends_past_the_last_word_that_runs_on_from_the_caret() {
    let mut g = Grid::blank(4, 60);
    // A bare line: the line ends at the cell past the caret.
    g.write(1, "$ ls");
    assert_eq!(line_past(&RULE, &scene(&g, (1, 4))), 5);
    // A suggestion runs on from the caret, its words a space apart: the line ends past its
    // last letter.
    g.write(1, SUGGESTION);
    let end = i32::try_from(SUGGESTION.len()).unwrap();
    assert_eq!(line_past(&RULE, &scene(&g, (1, 2))), end);
    // Two spaces still join two words; three do not, and neither does the gap before a prompt
    // at the row's right end.
    let mut g = Grid::blank(4, 60);
    g.write(1, "$ ab  cd   ef");
    g.fill_row(1, 50..60);
    assert_eq!(line_past(&RULE, &scene(&g, (1, 2))), 8);
    // The caret on the pane's last column: nothing is past it.
    assert_eq!(line_past(&RULE, &scene(&g, (1, 59))), 60);
}

/// A shell's grey suggestion (fish, zsh-autosuggestions) appears past the caret, on the pet's
/// seat. It is the hand's line, not a wall: the pet goes on past its end at once and is beside
/// the line there, and once the suggestion is gone it comes back along the row a dwell later.
/// Costed from the caret, the seat past the suggestion (38 cells on) lost to off the glass (26)
/// and the pet left the glass, by a cut, at the first suggestion of every command.
#[test]
fn a_suggestion_past_the_caret_sends_the_pet_past_its_end_and_never_off_the_glass() {
    let mut g = prompt_page();
    let caret = (13, 2);
    let (rest, _) = best(&RULE, &scene(&g, caret));
    assert!(matches!(rest, Seat::On { row: 13, .. }), "{rest:?}");
    let mut state = decide(&RULE, &scene(&g, caret), sat(rest), 16).state;
    // The suggestion is printed.
    g.write(13, SUGGESTION);
    let end = u16::try_from(SUGGESTION.len()).unwrap();
    let s = scene(&g, caret);
    assert!(!legal(&RULE, &s, rest), "the suggestion is on the seat");
    let d = decide(&RULE, &s, state, 16);
    let Seat::On { row, col, .. } = d.state.seat else {
        panic!("off the glass for a suggestion: {d:?}");
    };
    assert_eq!((d.why, row), (Why::GaveWay, 13), "{d:?}");
    assert!(
        col > end && col <= end + RULE.lead + 1,
        "{d:?}: its end is {end}"
    );
    assert_eq!(
        d.state.strikes, 0,
        "no letter landed on it: the line ran on"
    );
    assert_eq!(path(&RULE, &s, rest, d.state.seat), Path::Leap);
    assert!(cost(&RULE, &s, d.state.seat) < cost(&RULE, &s, Seat::Off(Edge::Left)));
    // While it shows, the seat past its end is kept: nothing is better.
    state = d.state;
    for _ in 0..40 {
        let d = decide(&RULE, &s, state, 16);
        assert_eq!(d.why, Why::Kept, "{d:?}");
        state = d.state;
    }
    // A key: the caret steps into the suggestion, whose end stays where it was. The pet stays.
    let typing = Scene {
        ahead: 3,
        ..scene(&g, (13, 3))
    };
    let d = decide(&RULE, &typing, state, 16);
    assert!(matches!(d.state.seat, Seat::On { row: 13, .. }), "{d:?}");
    // The suggestion goes: the pet is back beside the caret a dwell later, by a walk.
    let mut g = prompt_page();
    g.write(13, "$ g");
    let s = scene(&g, (13, 3));
    let far = d.state.seat;
    state = d.state;
    let mut back = None;
    for frame in 0..40u32 {
        let d = decide(&RULE, &s, state, 16);
        if d.why != Why::Kept {
            back = Some((frame * 16, d));
            break;
        }
        state = d.state;
    }
    let (after_ms, d) = back.expect("it comes back along the row");
    assert!(after_ms + 16 >= RULE.dwell_ms, "back after {after_ms} ms");
    assert_eq!(d.why, Why::Moved, "{d:?}");
    assert_eq!(path(&RULE, &s, far, d.state.seat), Path::Walk);
    assert_eq!(
        cost(&RULE, &s, d.state.seat),
        cost(&RULE, &s, best(&RULE, &s).0)
    );
}

/// Claude Code's input box, as the trail film's `composer` draws it: a rule over the typed row
/// and one under it, the program's footer under that.
fn composer_page() -> Grid {
    let mut g = Grid::blank(14, 72);
    g.write(
        4,
        "  this box. Type a line into either to judge the trail on it.",
    );
    g.write(6, "* Worked for 42s");
    g.rule(9);
    g.write(10, "> echo the quick bro");
    g.rule(11);
    g.write(12, "  ? for shortcuts");
    g.write(13, "  >> accept edits on");
    g
}

/// No seat is sought across a rule. In the composer the whole drawing finds no room on the
/// typed row (the rule is over it), and three rows down, under the box, it does: 8 against the
/// small drawing's 9 beside the caret. The pet then sat in the program's footer, off the line a
/// person types on, for 207 of the film's 379 frames. With the rule a fence it is drawn small
/// on the typed row, past the caret.
#[test]
fn in_an_input_box_the_pet_keeps_to_the_typed_row() {
    let g = composer_page();
    let typing = Scene {
        ahead: 3,
        ..scene(&g, (10, 20))
    };
    for s in [scene(&g, (10, 20)), typing] {
        let (seat, _) = best(&RULE, &s);
        let Seat::On { row, col, small } = seat else {
            panic!("off the glass in the composer: {seat:?}");
        };
        assert_eq!((row, small), (10, true), "{seat:?}");
        assert!(
            col > 20 + s.ahead && col <= 21 + RULE.lead.max(s.ahead + 1),
            "{seat:?}"
        );
    }
    // The control: the same page with its rules read as letters only and every row at one
    // price seats the whole pet in the footer. Each of the two keeps it on the typed row
    // alone: the price where a program draws no rule (rows of text over and under a line), the
    // fence where the typed row's own seats are far along it.
    let mut open = g.clone();
    open.fences.clear();
    let flat = Rule {
        far_row: RULE.row,
        ..RULE
    };
    assert_eq!(best(&flat, &scene(&open, (10, 20))).0, on(13, 21));
    assert_eq!(best(&flat, &scene(&g, (10, 20))).0, small(10, 25));
    assert_eq!(best(&RULE, &scene(&open, (10, 20))).0, small(10, 25));
    let mut long = g.clone();
    long.fill_row(10, 2..68);
    let mut open = long.clone();
    open.fences.clear();
    assert!(matches!(
        best(&RULE, &scene(&open, (10, 68))).0,
        Seat::On { row: 12..=13, .. }
    ));
    assert!(!best(&RULE, &scene(&long, (10, 68))).0.on_page());
    // A rule over the line fences the rows over it as one under it does the rows under.
    let mut g = Grid::blank(8, 40);
    g.rule(3);
    g.write(4, "> ab");
    g.fill_row(4, 6..40);
    let (seat, _) = best(&RULE, &scene(&g, (4, 4)));
    assert!(
        !matches!(seat, Seat::On { row, .. } if row < 4),
        "{seat:?} is across the rule over the line"
    );
}

/// A page that scrolled under a seat beside a caret that stayed brought its letters up under
/// the seat: none landed on it. The pet goes at once to the best seat there is, on the glass,
/// and no strike is held against it. The same letters with no scroll are a letter's landing.
#[test]
fn letters_a_scroll_brings_under_the_seat_take_the_pet_to_the_best_seat_not_off_the_glass() {
    let mut g = Grid::blank(14, 72);
    g.write(13, "$");
    let caret = (13, 2);
    // Seated whole on the last row: its box reaches the row over it.
    let seat = on(13, 7);
    assert!(legal(&RULE, &scene(&g, caret), seat));
    let state = decide(&RULE, &scene(&g, caret), sat(seat), 16).state;
    // A line of output is printed and the page scrolls: it is now on the row over the prompt.
    g.write(
        12,
        "   Compiling aterm-effects v0.0.0 (crates/aterm-effects)",
    );
    let scrolled = Scene {
        scrolled: true,
        ..scene(&g, caret)
    };
    let d = decide(&RULE, &scrolled, state, 16);
    assert_eq!(d.why, Why::GaveWay, "{d:?}");
    assert!(d.state.seat.on_page(), "{d:?}");
    assert_eq!((d.state.strikes, d.state.quiet_owed), (0, false), "{d:?}");
    assert_clear(&scrolled, d.state.seat, "after the scroll");
    // The control.
    let landed = decide(&RULE, &scene(&g, caret), state, 16);
    assert_eq!(landed.why, Why::Yielded, "{landed:?}");
    assert_eq!(landed.state.strikes, 1);
    // A seat the scroll left clear is kept: a scroll alone moves nothing.
    let small_seat = small(13, 7);
    let kept = decide(&RULE, &scrolled, sat(small_seat), 16);
    assert_eq!((kept.why, kept.state.seat), (Why::Kept, small_seat));
}

/// The decision says whether a better seat is waiting on a clock, which is what a host that
/// draws on demand is woken by.
#[test]
fn a_decision_says_whether_a_better_seat_is_waiting() {
    let g = Grid::blank(6, 60);
    let s = scene(&g, (3, 10));
    let (rest, _) = best(&RULE, &s);
    assert!(!decide(&RULE, &s, sat(rest), 16).better);
    // Far along the row: the seat at the lead's end is better, and the dwell has not run.
    let d = decide(&RULE, &s, sat(on(3, 50)), 16);
    assert!(d.better && d.why == Why::Kept, "{d:?}");
    // Off the glass at the first asking, no time gone by: a seat is better already.
    let d = decide(&RULE, &s, SeatState::offstage(Edge::Right), 0);
    assert!(d.better && d.state.beaten_ms == 0, "{d:?}");
    // The move itself waits on nothing.
    let d = decide(&RULE, &s, sat(on(3, 50)), RULE.dwell_ms);
    assert!(d.why == Why::Moved && !d.better, "{d:?}");
}
