// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

const CW: i32 = 9;
const CH: i32 = 18;
/// The shipped footprint at `ch = 18`: ~6 cells wide, 1.7 `ch` tall.
const BODY_W: u16 = 54;
const BODY_H: u16 = 30;
/// `cw · KITTY_LEAD_NUM / KITTY_LEAD_DEN` at `cw = 9`.
const LEAD_PX: i32 = CW * 3 / 4;

const fn geom_cols(cols: usize) -> Geom {
    Geom {
        cw: CW as usize,
        ch: CH as usize,
        rows: 40,
        cols,
        origin_x: 0,
        origin_y: 0,
        win_w: 1080,
        win_h: 720,
        head: 0,
    }
}

/// The shipped seat as a fixture: `kitty_cursor_footprint`'s ¾-cell lead,
/// right-margin clamp and boundary rise, bob 0.
const fn footprint(caret: (u16, u16), cols: u16) -> CatFootprint {
    let grid_w = cols as i32 * CW;
    let cursor_right = (caret.1 as i32 + 1) * CW;
    let x_cand = cursor_right + LEAD_PX;
    let x_max = grid_w - BODY_W as i32;
    let x = if x_cand < x_max { x_cand } else { x_max };
    assert!(x >= 0, "fixture: the grid is narrower than the cat");
    let rest_top = caret.0 as i32 * CH + CH / 2 - BODY_H as i32 / 2;
    let intrusion = cursor_right - x;
    let rise = if intrusion > 0 {
        let cap1 = if intrusion < CH / 2 {
            intrusion
        } else {
            CH / 2
        };
        let cap2 = if rest_top > 0 { rest_top } else { 0 };
        if cap1 < cap2 { cap1 } else { cap2 }
    } else {
        0
    };
    CatFootprint {
        x,
        y: rest_top - rise,
        w: BODY_W,
        h: BODY_H,
    }
}

/// Typed up to the margin, the head at a line's end rises into the row
/// above once and stays; mid-line, a held sky keeps it up. Negative
/// control: unheld, the head goes down and up with the keys.
#[test]
fn at_the_line_s_end_the_head_rises_once_and_stays() {
    let walk = |held: bool, margin: bool| -> Vec<(u16, u16, Side)> {
        let mut held_sky = false;
        let mut seats = Vec::new();
        for caret_col in 110..120_u16 {
            let caret = (7_u16, caret_col);
            let reach = if margin { 7 } else { 9 };
            let ink = |r: u16, c: u16| Some(r == 7 && c + reach < caret_col);
            let s = placement(
                caret,
                &SeatQuery {
                    held_sky: held && held_sky,
                    ..seat_query(caret, 120)
                },
                ink,
            );
            held_sky = s.lift_rows > 0 && !s.over_ink;
            seats.push((caret_col, s.lift_rows, s.side));
        }
        seats
    };
    let seats = walk(true, true);
    assert!(
        seats
            .iter()
            .all(|&(_, lift, side)| lift == 1 || side == Side::Ahead),
        "the head took the mirror at the line's end: {seats:?}"
    );
    let rose = seats.iter().position(|s| s.1 == 1).expect("it rises");
    assert!(
        seats[rose..].iter().all(|s| s.1 == 1),
        "it came down again before the line's end: {seats:?}"
    );
    let far = walk(true, false);
    assert!(
        far.iter()
            .any(|&(_, lift, side)| lift == 0 && side == Side::Behind),
        "a line not written near the caret keeps the mirror at the margin: {far:?}"
    );

    let mid = |held: bool| -> Vec<u16> {
        let mut held_sky = false;
        let mut lifts = Vec::new();
        for caret_col in 40..60_u16 {
            let caret = (7_u16, caret_col);
            let ink = |r: u16, c: u16| {
                Some(r == 7 && (c > caret_col || (caret_col % 2 == 1 && c < caret_col)))
            };
            let s = placement(
                caret,
                &SeatQuery {
                    held_sky: held && held_sky,
                    ..seat_query(caret, 120)
                },
                ink,
            );
            held_sky = s.lift_rows > 0 && !s.over_ink;
            lifts.push(s.lift_rows);
        }
        lifts
    };
    let held = mid(true);
    let first = held.iter().position(|&l| l == 1).expect("it rises");
    assert!(
        held[first..].iter().all(|&l| l == 1),
        "mid-line, the held sky came down to the mirror: {held:?}"
    );
    assert!(
        mid(false).windows(2).any(|w| w[0] != w[1]),
        "negative control: unheld, the head goes down and up with the keys"
    );
}

/// A head past a line's last letter reads the row's unwritten columns blank
/// ([`cell_ink`]) and rises into the row above. Negative control: read as
/// unknown, they blind the probe and the head sits behind the caret.
#[test]
fn a_head_past_the_last_letter_reads_the_unwritten_columns_blank() {
    use aterm_core::terminal::RenderCell;
    let cols = 120_u16;
    let caret = (7_u16, 114_u16);
    let letter = RenderCell {
        ch: 'x',
        ..RenderCell::default()
    };
    // Row 7 written up to the caret and no further; the rows above carry no cells.
    let mut cells: Vec<Vec<RenderCell>> = vec![Vec::new(); 12];
    cells[7] = vec![letter; usize::from(caret.1)];
    let query = seat_query(caret, usize::from(cols));
    let (first, _) = cell_span(query.rest.x, i32::from(query.rest.w), CW, cols);
    assert_eq!(
        first, caret.1,
        "fixture: the head's rest seat starts at the caret, past the last letter"
    );
    let seat = placement(caret, &query, |r, c| cell_ink(&cells, r, c, cols));
    assert_eq!(
        (seat.lift_rows, seat.over_ink, seat.row),
        (1, false, caret.0 - 1),
        "the head must rise clear into the row above: {seat:?}"
    );
    let blind = placement(caret, &query, |r, c| {
        cells
            .get(usize::from(r))
            .and_then(|line| line.get(usize::from(c)))
            .map(|cell| cell.ch != ' ')
    });
    let (a, b) = covered(&blind);
    assert!(
        blind.lift_rows == 0 && blind.side == Side::Behind && b < caret.1 && a < b,
        "negative control: read as unknown, the unwritten columns must blind the probe and \
         seat the head behind the caret on the letters: {blind:?}"
    );
    assert_eq!(cell_ink(&cells, 7, 113, cols), Some(true));
    assert_eq!(cell_ink(&cells, 7, 119, cols), Some(false));
    assert_eq!(cell_ink(&cells, 7, 120, cols), None, "past the grid");
    assert_eq!(cell_ink(&cells, 12, 0, cols), None, "a row the frame lacks");
}

fn seat_query(caret: (u16, u16), cols: usize) -> SeatQuery {
    SeatQuery {
        geom: geom_cols(cols),
        rest: footprint(caret, u16::try_from(cols).expect("fixture cols")),
        lead_px: LEAD_PX,
        held_sky: false,
    }
}

const fn admitted(duty: CompanionDuty) -> CompanionAdmission {
    CompanionAdmission {
        duty,
        body_claimed: false,
    }
}

/// Blank glass everywhere.
const fn blank(_row: u16, _col: u16) -> Option<bool> {
    Some(false)
}

/// The inclusive cells a seat covers, from its final `x`.
const fn covered(s: &Seat) -> (u16, u16) {
    cell_span(s.x, s.w as i32, CW, 120)
}

/// The router names one body, the host duty's, and a frame whose body is
/// already claimed draws nothing.
#[test]
fn a_claimed_frame_draws_no_second_body() {
    for (duty, want) in [
        (CompanionDuty::Idle, Body::None),
        (CompanionDuty::Pet, Body::Pet),
        (
            CompanionDuty::FlyingHead { cell: (2, 40) },
            Body::Flying { cell: (2, 40) },
        ),
    ] {
        assert_eq!(body_for(admitted(duty)), want, "{duty:?}");
        assert_eq!(
            body_for(CompanionAdmission {
                duty,
                body_claimed: true,
            }),
            Body::None,
            "{duty:?}: a claimed frame must never mint a second body"
        );
    }
}

/// D13: the flying head takes the meteor whole; the pet is offered the
/// arrival edge and nothing else (no teleport, spine impulse or retiming,
/// and no router-held cadence for the pet's own latch).
#[test]
fn the_pet_is_offered_the_meteor_edge_but_keeps_its_own_pounce() {
    let t0 = Instant::now();
    let t_flight = Duration::from_millis(100);
    let imp = CompanionImpulse::Meteor {
        dir: Dir::Right,
        t0,
        t_flight,
    };
    let land = t0.checked_add(t_flight).expect("100 ms fits");
    let head = Body::Flying { cell: (7, 80) };

    let BodyImpulse::Fly(flight) = impulse_for(head, imp) else {
        panic!("the flying head takes the meteor");
    };
    assert_eq!(flight.land_at, land, "the arrival edge is minted once");
    assert_eq!(flight.land, (7, 80), "the snap-to is the escorted cell");
    assert!(flight.disp_floor() >= 0.97, "§7.2: disp = max(disp, 0.97)");

    let pet = impulse_for(Body::Pet, imp);
    assert_eq!(
        pet,
        BodyImpulse::Perk { at: land },
        "the pet is offered t0 + T as its perk edge and nothing more"
    );
    assert!(
        !matches!(pet, BodyImpulse::Fly(_)),
        "a v2 meteor must never relocate the pet (owner Q14: no teleport)"
    );

    assert_eq!(impulse_for(Body::None, imp), BodyImpulse::Ignore);
    for (raw, want) in [
        (CompanionImpulse::Land, Reaction::Land),
        (CompanionImpulse::Wince, Reaction::Wince),
        (CompanionImpulse::Delight, Reaction::Delight),
    ] {
        assert_eq!(impulse_for(Body::Pet, raw), BodyImpulse::React(want));
        assert_eq!(impulse_for(head, raw), BodyImpulse::React(want));
        assert_eq!(
            impulse_for(Body::None, raw),
            BodyImpulse::Ignore,
            "a reaction with no body is not a pose"
        );
    }
}

/// Whatever the caret column and ink, the seat never covers the caret
/// cell or an avoidable inked cell, keeps the shipped seat on blank glass
/// wherever it clears the caret, yields behind where the margin clamp makes
/// it intrude, and carries one position.
#[test]
fn the_flying_head_never_covers_the_cell_the_caret_is_typing_into() {
    let caret_row = 7_u16;
    let inked_line = |row: u16, col: u16| -> Option<bool> {
        if row == caret_row {
            Some(col < 40)
        } else {
            Some(false)
        }
    };
    let mut intruded = 0_u32;
    for caret_col in 0..120_u16 {
        let caret = (caret_row, caret_col);
        let s = placement(caret, &seat_query(caret, 120), inked_line);
        let (first, last) = covered(&s);
        assert!(
            s.row != caret_row || !(first..=last).contains(&caret_col),
            "caret_col {caret_col}: the seat covered the caret cell \
             (x {}, row {})",
            s.x,
            s.row
        );
        if s.row == caret_row && !s.over_ink {
            for c in first..=last {
                assert_eq!(
                    inked_line(caret_row, c),
                    Some(false),
                    "caret_col {caret_col}: the seat covered inked cell {c}"
                );
            }
        }
        assert!(
            ((s.col * CW as f32).round() as i32 - s.x).abs() <= 1,
            "caret_col {caret_col}: `col` disagrees with `x`"
        );
        let blank_seat = placement(caret, &seat_query(caret, 120), blank);
        let shipped = footprint(caret, 120);
        let (s_first, s_last) = cell_span(shipped.x, i32::from(shipped.w), CW, 120);
        assert_eq!(blank_seat.row, caret_row);
        assert_eq!(blank_seat.lift_rows, 0);
        assert!(!blank_seat.over_ink);
        if (s_first..=s_last).contains(&caret_col) {
            intruded += 1;
            let (first, last) = covered(&blank_seat);
            assert!(
                !(first..=last).contains(&caret_col),
                "caret_col {caret_col}: the margin seat covered the caret"
            );
            assert_eq!(blank_seat.side, Side::Behind);
            assert_eq!(
                blank_seat.x,
                i32::from(caret_col) * CW - LEAD_PX - i32::from(BODY_W),
                "caret_col {caret_col}: the margin yields to the mirror seat"
            );
        } else {
            assert_eq!(
                (blank_seat.x, blank_seat.y, blank_seat.w, blank_seat.h),
                (shipped.x, shipped.y, shipped.w, shipped.h),
                "caret_col {caret_col}: blank glass must keep the shipped seat"
            );
        }
    }
    assert!(
        intruded > 0,
        "fixture: the margin clamp must intrude somewhere for the arm to count"
    );

    let mid = placement((caret_row, 5), &seat_query((caret_row, 5), 120), inked_line);
    assert_eq!(mid.row, caret_row - 1, "the eraser case lifts one row");
    assert_eq!(mid.lift_rows, 1);
    assert!(!mid.over_ink);

    let back = placement(
        (caret_row, 38),
        &seat_query((caret_row, 38), 120),
        |r, c| Some(r == caret_row && c >= 39),
    );
    assert_eq!(back.side, Side::Behind);
    assert_eq!(back.row, caret_row);
    assert_eq!(
        back.x,
        38 * CW - LEAD_PX - i32::from(BODY_W),
        "the mirror keeps the shipped lead's clearance"
    );
    assert_eq!(
        back.y,
        footprint((caret_row, 38), 120).y,
        "the row's shipped rest"
    );
}

/// Seat steps 3 and 4: a fully inked row lifts the body into the sky; a
/// fully inked sky draws over the words there; a top-row caret stays on its
/// row without covering the caret; a grid too narrow for either side falls
/// back to the shipped footprint.
#[test]
fn a_crowded_line_yields_to_the_sky_then_draws_over_the_words() {
    let all_ink = |_r: u16, _c: u16| Some(true);
    let sky_only = |r: u16, _c: u16| Some(r != 6);

    let lifted = placement((7, 5), &seat_query((7, 5), 120), sky_only);
    assert_eq!(lifted.row, 6, "a crowded line lifts one row");
    assert_eq!(lifted.lift_rows, 1);
    assert!(!lifted.over_ink, "a clear sky needs no over-exception");
    assert_eq!(
        lifted.y,
        footprint((7, 5), 120).y - CH,
        "the sky seat is the shipped rest moved up exactly one row"
    );

    let buried = placement((7, 5), &seat_query((7, 5), 120), all_ink);
    assert_eq!(
        buried.row, 6,
        "over the words in the sky, never the caret row"
    );
    assert!(buried.over_ink);

    let top = placement((0, 5), &seat_query((0, 5), 120), all_ink);
    assert_eq!(top.row, 0);
    assert_eq!(top.lift_rows, 0);
    assert!(top.over_ink);
    let (first, last) = covered(&top);
    assert!(
        !(first..=last).contains(&5),
        "row 0 must not cover the caret"
    );
    assert_eq!(top.side, Side::Ahead);

    let wall = placement((0, 117), &seat_query((0, 117), 120), all_ink);
    assert_eq!(wall.row, 0);
    assert_eq!(wall.side, Side::Behind);
    let (first, last) = cell_span(wall.x, i32::from(wall.w), CW, 120);
    assert!(
        !(first..=last).contains(&117),
        "row 0 at the wall must not cover the caret"
    );

    let narrow = placement((0, 4), &seat_query((0, 4), 10), all_ink);
    let shipped = footprint((0, 4), 10);
    assert_eq!((narrow.x, narrow.y), (shipped.x, shipped.y));
    assert!(narrow.over_ink);
}

/// A blind host (every probe `None`) keeps the shipped seat wherever it clears the caret and
/// never gets the sky or an over-ink verdict; at the right margin it takes the mirror seat,
/// and where neither side clears, the shipped footprint.
#[test]
fn an_ink_blind_host_keeps_the_shipped_seat_but_never_the_caret_cell() {
    let blind_probe = |_r: u16, _c: u16| -> Option<bool> { None };
    for caret in [(7_u16, 5_u16), (7, 60), (0, 5)] {
        let blind = placement(caret, &seat_query(caret, 120), blind_probe);
        let shipped = footprint(caret, 120);
        assert_eq!(
            (blind.x, blind.y, blind.row, blind.lift_rows, blind.over_ink),
            (shipped.x, shipped.y, caret.0, 0, false),
            "{caret:?}: a blind host must keep the shipped seat"
        );
    }

    let caret = (7_u16, 117_u16);
    let shipped = footprint(caret, 120);
    let (s_first, s_last) = cell_span(shipped.x, i32::from(shipped.w), CW, 120);
    assert!(
        (s_first..=s_last).contains(&caret.1),
        "fixture: the shipped seat must sit on the caret cell at the wall"
    );
    let wall = placement(caret, &seat_query(caret, 120), blind_probe);
    let (first, last) = covered(&wall);
    assert!(
        !(first..=last).contains(&caret.1),
        "a blind host must still keep off the caret cell (x {}, cells {first}..={last})",
        wall.x
    );
    assert_eq!(
        wall.x,
        i32::from(caret.1) * CW - LEAD_PX - i32::from(BODY_W),
        "…by taking the mirror seat, the shipped lead's clearance back"
    );
    assert_eq!(
        (wall.y, wall.row, wall.lift_rows, wall.side, wall.over_ink),
        (shipped.y, caret.0, 0, Side::Behind, false),
        "…on the caret's own row: no sky and no over-ink verdict from a \
         probe that answered nothing"
    );
    let sighted = placement(caret, &seat_query(caret, 120), blank);
    assert_eq!(
        (wall.x, wall.y, wall.row, wall.side),
        (sighted.x, sighted.y, sighted.row, sighted.side),
        "blind and sighted must agree wherever only the caret cell decides"
    );

    let narrow = placement((0, 4), &seat_query((0, 4), 10), blind_probe);
    let shipped = footprint((0, 4), 10);
    assert_eq!(
        (
            narrow.x,
            narrow.y,
            narrow.row,
            narrow.lift_rows,
            narrow.over_ink
        ),
        (shipped.x, shipped.y, 0, 0, false),
        "too narrow for either side: today's footprint, no over-ink verdict"
    );

    let mixed = placement((7, 5), &seat_query((7, 5), 120), |r, c| {
        (r != 7 || c != 8).then_some(false)
    });
    assert_ne!(mixed.row, 7, "a single unknowable cell in the span yields");
}

/// A STAR WHOSE ARM REACHES THE PET'S BODY IS NOT DRAWN ON IT (looks8 K8):
/// a star beside the head whose centre misses the body, its arms across
/// the paper cat's eye, is shaded at its peak reach, and one whose reach
/// stops short of the fur is drawn. The shaved transparent border holds:
/// a reach into the tile's margin alone touches no fur. Negative
/// control: keyed on the centre alone (no reach), the star beside the
/// head is drawn.
#[test]
fn a_star_whose_arm_reaches_the_pet_is_shaded_whole() {
    let cw = 14usize;
    let border = (SHADE_BORDER_CELLS * cw as f32).round() as i32;
    let body = (900, 960, 88, 118);
    let mut shade = PetShade::EMPTY;
    shade.note(Some(body));
    let beside_eye = (body.0 + border - 3, 95);
    assert!(
        shade.hides(beside_eye, 4, cw),
        "an arm across the eye is shaded"
    );
    assert!(
        !shade.hides(beside_eye, 2, cw),
        "a reach into the tile's margin alone touches no fur"
    );
    assert!(
        !shade.hides((body.0 + border - 8, 95), 4, cw),
        "a star clear of the fur stays"
    );
    assert!(
        !shade.hides(beside_eye, 0, cw),
        "control: keyed on its centre, the star on the eye is drawn"
    );
    assert!(
        shade.hides((930, 100), 0, cw),
        "a centre on the body is shaded"
    );
}

/// A mark that reaches a box is kept off it and one beside it is not; a body under a box
/// reaches no higher than its row's top there, easing back beside it. Negative control: an
/// empty keep-out keeps nothing.
#[test]
fn the_keep_out_holds_marks_and_the_body_s_top_off_its_boxes() {
    let mut k = KeepOut::default();
    // A `fox` face over cells 4..6 of row 2 (16-px rows), and an empty box.
    k.set([(32, 56, 32, 48), (10, 10, 0, 5)]);
    assert_eq!(k.boxes().len(), 1, "an empty box is no box");
    assert!(k.covers((44, 40), 0), "a star on the face is kept off");
    assert!(
        k.covers((30, 40), 3),
        "a star whose arm reaches the face is kept off"
    );
    assert!(!k.covers((20, 40), 3), "a star clear of the face is drawn");
    let (row_top, free, ease) = (48.0, 40.0, 6.0);
    assert_eq!(
        k.ceiling(44.0, row_top, free, ease),
        row_top,
        "no feather under the box"
    );
    let beside = k.ceiling(59.0, row_top, free, ease);
    assert!(
        free < beside && beside < row_top,
        "the ceiling eases beside it ({beside})"
    );
    assert_eq!(
        k.ceiling(80.0, row_top, free, ease),
        free,
        "far off, the body's own reach"
    );
    assert_eq!(
        k.ceiling(44.0, 64.0, free, ease),
        free,
        "a row the box does not stand on"
    );
    assert_eq!(
        k.below((40, 48), (40, 56)),
        (48, 56),
        "a column under the box keeps only its span below it"
    );
    assert_eq!(
        k.below((40, 48), (40, 47)),
        (47, 47),
        "a column wholly in the box keeps nothing"
    );
    assert_eq!(
        k.below((60, 70), (40, 56)),
        (40, 56),
        "a column beside it keeps all"
    );
    let none = KeepOut::default();
    assert!(!none.covers((44, 40), 0), "control: nothing kept");
    assert_eq!(
        none.below((40, 48), (40, 56)),
        (40, 56),
        "control: nothing cut"
    );
    assert_eq!(
        none.ceiling(44.0, row_top, free, ease),
        free,
        "control: no ceiling"
    );
}
