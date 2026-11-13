// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::peer_check::{self, Custody, Listing, Read, RosterGate, Stage, View};
use super::{peer_check_next, peer_check_roster, peer_check_start, peer_check_view, seat_out};

const ISLAND: &str = include_str!("peer_check_island.rs");

/// The literal the island proves the sheet `name` equals.
fn proved(name: &str) -> &'static str {
    let key = format!("{name} = \"");
    let start = ISLAND
        .find(&key)
        .unwrap_or_else(|| panic!("the island proves no {name}"))
        + key.len();
    let len = ISLAND[start..]
        .find('"')
        .expect("the sheet's closing quote");
    &ISLAND[start..start + len]
}

fn digit(code: u8) -> char {
    assert!(code < 10, "a code past one digit: {code}");
    char::from(b'0' + code)
}

/// The start sheet: custody 0 to 2.
fn start_sheet(start: impl Fn(u8) -> u8) -> String {
    (0..3).map(|c| digit(start(c))).collect()
}

/// The next sheet: custody 0 to 2, then stage 0 to 6, then outcome 0 to 4, then the witness
/// false and true, as the island's `sheetNext` folds them.
fn next_sheet(next: impl Fn(u8, u8, u8, bool) -> u8) -> String {
    let mut sheet = String::new();
    for c in 0..3 {
        for s in 0..7 {
            for o in 0..5 {
                for w in [false, true] {
                    sheet.push(digit(next(c, s, o, w)));
                }
            }
        }
    }
    sheet
}

/// The roster sheet: custody, roster read and deny list read, each 0 to 2.
fn roster_sheet(roster: impl Fn(u8, u8, u8) -> u8) -> String {
    let mut sheet = String::new();
    for c in 0..3 {
        for r in 0..3 {
            for d in 0..3 {
                sheet.push(digit(roster(c, r, d)));
            }
        }
    }
    sheet
}

/// The view sheet: custody 0 to 2, then outcome 0 to 4.
fn view_sheet(view: impl Fn(u8, u8) -> u8) -> String {
    let mut sheet = String::new();
    for c in 0..3 {
        for o in 0..5 {
            sheet.push(digit(view(c, o)));
        }
    }
    sheet
}

/// The removal predicate's sheet: the actor, recipient, writer and kin verdicts, each false
/// then true, as the island's `sheetSeatOut` folds them.
fn seat_sheet(out: impl Fn(bool, bool, bool, bool) -> bool) -> String {
    let mut sheet = String::new();
    for a in [false, true] {
        for r in [false, true] {
            for w in [false, true] {
                for k in [false, true] {
                    sheet.push(digit(u8::from(out(a, r, w, k))));
                }
            }
        }
    }
    sheet
}

#[test]
fn the_kernels_answer_the_islands_sheets() {
    assert_eq!(start_sheet(peer_check_start), proved("sheetStart"));
    assert_eq!(next_sheet(peer_check_next), proved("sheetNext"));
    assert_eq!(roster_sheet(peer_check_roster), proved("sheetRoster"));
    assert_eq!(view_sheet(peer_check_view), proved("sheetView"));
    assert_eq!(seat_sheet(seat_out), proved("sheetSeatOut"));
}

/// NEGATIVE CONTROL: a removal predicate that lost its actor disjunct (the island's
/// `seatOutNoActor`) answers another sheet, and keeps the seat whose actor alone is out.
#[test]
fn the_removal_predicate_without_its_actor_answers_another_sheet() {
    let no_actor = |_: bool, r: bool, w: bool, k: bool| seat_out(false, r, w, k);
    assert_ne!(seat_sheet(no_actor), proved("sheetSeatOut"));
    assert!(seat_out(true, false, false, false));
    assert!(!no_actor(true, false, false, false));
}

/// Every code outside the meaningful domain refuses, each coordinate across its whole range
/// while the others range over theirs (the island proves it for every value at once), and
/// every decoder takes a code outside its enum to a refusal.
#[test]
fn every_code_outside_the_domain_refuses() {
    let refuse = Stage::Refuse as u8;
    for x in 0..=u8::MAX {
        if x > Custody::Log as u8 {
            assert_eq!(peer_check_start(x), refuse);
        }
        for a in 0..3 {
            for b in 0..7 {
                for w in [false, true] {
                    if x > Custody::Log as u8 {
                        assert_eq!(peer_check_next(x, b, a, w), refuse);
                    }
                    if x > Stage::Refuse as u8 {
                        assert_eq!(peer_check_next(a, x, b.min(4), w), refuse);
                    }
                    if x > Read::Out as u8 {
                        assert_eq!(peer_check_next(a, b, x, w), refuse);
                    }
                }
                if b <= 2 && x > 2 {
                    assert_eq!(peer_check_roster(x, a, b), RosterGate::Refuse as u8);
                    assert_eq!(peer_check_roster(a, x, b), RosterGate::Refuse as u8);
                    assert_eq!(peer_check_roster(a, b, x), RosterGate::Refuse as u8);
                }
                if b <= 4 && x > 2 {
                    assert_eq!(peer_check_view(x, b), View::Refuse as u8);
                }
            }
            if x > Read::Out as u8 {
                assert_eq!(peer_check_view(a, x), View::Refuse as u8);
            }
        }
        if x > Stage::Refuse as u8 {
            assert_eq!(Stage::of(x), Stage::Refuse);
        }
        if x > RosterGate::Check as u8 {
            assert_eq!(RosterGate::of(x), RosterGate::Refuse);
        }
        if x > View::Held as u8 {
            assert_eq!(View::of(x), View::Refuse);
        }
    }
}

/// Each enum's discriminant is the code the island names, and its decoder reads it back.
#[test]
fn the_enums_are_the_islands_codes() {
    let stages = [
        Stage::Idle,
        Stage::Roster,
        Stage::Held,
        Stage::Acknowledged,
        Stage::Combined,
        Stage::Pass,
        Stage::Refuse,
    ];
    for (code, stage) in (0..).zip(stages) {
        assert_eq!((stage as u8, Stage::of(code)), (code, stage));
    }
    for (code, gate) in (0..).zip([RosterGate::Refuse, RosterGate::Nothing, RosterGate::Check]) {
        assert_eq!((gate as u8, RosterGate::of(code)), (code, gate));
    }
    for (code, view) in (0..).zip([View::Refuse, View::None, View::Held]) {
        assert_eq!((view as u8, View::of(code)), (code, view));
    }
    for (code, custody) in (0..).zip([Custody::None, Custody::Journal, Custody::Log]) {
        assert_eq!(custody as u8, code);
    }
    for (code, read) in (0..).zip([
        Read::Unread,
        Read::Absent,
        Read::Foreign,
        Read::Clear,
        Read::Out,
    ]) {
        assert_eq!(read as u8, code);
    }
    for (code, listing) in (0..).zip([Listing::Unread, Listing::Empty, Listing::Names]) {
        assert_eq!(listing as u8, code);
    }
}

/// The codes read as the handshake reads them, at the cells the loopback tests rely on.
#[test]
fn the_codes_name_the_cells_the_driver_reads() {
    use peer_check::{next, roster, start, view};
    assert_eq!(start(Custody::None), Stage::Pass);
    assert_eq!(start(Custody::Log), Stage::Roster);
    let at_log = |stage, read, witness| next(Custody::Log, stage, read, witness);
    assert_eq!(at_log(Stage::Roster, Read::Clear, false), Stage::Held);
    assert_eq!(at_log(Stage::Held, Read::Clear, false), Stage::Acknowledged);
    assert_eq!(
        at_log(Stage::Acknowledged, Read::Clear, true),
        Stage::Combined
    );
    assert_eq!(at_log(Stage::Combined, Read::Clear, true), Stage::Pass);
    for read in [Read::Unread, Read::Absent, Read::Foreign, Read::Out] {
        assert_eq!(at_log(Stage::Held, read, false), Stage::Refuse);
    }
    for read in [Read::Unread, Read::Foreign, Read::Out] {
        assert_eq!(at_log(Stage::Acknowledged, read, false), Stage::Refuse);
    }
    assert_eq!(at_log(Stage::Acknowledged, Read::Absent, true), Stage::Pass);
    let at_journal = |stage, read| next(Custody::Journal, stage, read, true);
    assert_eq!(at_journal(Stage::Roster, Read::Absent), Stage::Acknowledged);
    assert_eq!(at_journal(Stage::Acknowledged, Read::Unread), Stage::Pass);
    assert_eq!(at_journal(Stage::Acknowledged, Read::Out), Stage::Refuse);
    let (unread, empty, names) = (Listing::Unread, Listing::Empty, Listing::Names);
    assert_eq!(
        roster(Custody::Journal, unread, unread),
        RosterGate::Nothing
    );
    assert_eq!(roster(Custody::Log, unread, unread), RosterGate::Refuse);
    assert_eq!(roster(Custody::Log, empty, empty), RosterGate::Nothing);
    assert_eq!(roster(Custody::Log, names, empty), RosterGate::Check);
    assert_eq!(view(Custody::Log, Read::Clear), View::Held);
    assert_eq!(view(Custody::Journal, Read::Unread), View::None);
}

/// NEGATIVE CONTROLS: the sheets see each defect PF4 and X1 closed. The check before X1 (a
/// log endpoint's roster goes straight to the acknowledged log, whose failed read passes),
/// the held view's no-log and other-root cells continuing (the cells before PF4), an
/// acknowledged log of another root passing, and the roster gate reading a busy roster with an empty deny list
/// as nothing to check, each answer another sheet.
#[test]
fn a_kernel_with_an_old_cell_answers_another_sheet() {
    let (acknowledged, pass) = (Stage::Acknowledged as u8, Stage::Pass as u8);
    let pre_x1 = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
        (2, 1, 1 | 3) => acknowledged,
        (2, 3, 0) => pass,
        _ => peer_check_next(c, s, o, w),
    };
    let held_absent_continues = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
        (2, 2, 1 | 2) => acknowledged,
        _ => peer_check_next(c, s, o, w),
    };
    let acknowledged_foreign_passes = |c: u8, s: u8, o: u8, w: bool| match (c, s, o) {
        (2, 3, 2) => pass,
        _ => peer_check_next(c, s, o, w),
    };
    for mutant in [
        next_sheet(pre_x1),
        next_sheet(held_absent_continues),
        next_sheet(acknowledged_foreign_passes),
    ] {
        assert_ne!(mutant, proved("sheetNext"));
    }
    let roster_today = |c: u8, r: u8, d: u8| {
        if r == 0 && d == 1 {
            RosterGate::Nothing as u8
        } else {
            peer_check_roster(c, r, d)
        }
    };
    assert_ne!(roster_sheet(roster_today), proved("sheetRoster"));
    let view_unread_passes = |c: u8, o: u8| {
        if o == Read::Unread as u8 {
            View::None as u8
        } else {
            peer_check_view(c, o)
        }
    };
    assert_ne!(view_sheet(view_unread_passes), proved("sheetView"));
}
