// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::typed::cell::{Candidate, CellValue, Field, Projection, Row};
use aterm_json::json;

fn field(value: &str) -> Field {
    Field::Value(Candidate {
        operation: [1; 32],
        value: CellValue::owner(2, json!(value)),
    })
}

fn registry() -> Projection {
    let mut rows = Projection::default();
    rows.rows.insert(
        "mc-0000000000000007".into(),
        Row {
            fields: [
                ("label".into(), field("m7")),
                ("node".into(), field("0123456789abcdef")),
            ]
            .into(),
            ..Row::default()
        },
    );
    rows
}

fn seat() -> LabelSeat {
    LabelSeat {
        actor: [7; 48],
        node: "0123456789abcdef".into(),
        name: "log-name-seven".into(),
        mac: Some("mc-0000000000000007".into()),
        transport: "00112233445566778899aabbccddeeff".into(),
    }
}

/// The seat `word` names among `seats` for a drive or a read ([`LabelFor::Reach`]).
fn resolve<'a>(
    word: &str,
    machines: &Projection,
    seats: &'a [LabelSeat],
) -> Result<&'a LabelSeat, LabelRefused> {
    resolve_registry_label(word, machines, Ok(seats), LabelFor::Reach)
}

#[test]
fn label_follows_registry_and_uses_book_bound_old_transport() {
    let mut registry = registry();
    assert_eq!(
        resolve("m7", &registry, &[seat()]).unwrap().transport,
        "00112233445566778899aabbccddeeff"
    );
    let mut seat = seat();
    registry
        .rows
        .values_mut()
        .next()
        .unwrap()
        .fields
        .insert("label".into(), field("m22"));
    seat.transport = "n-0123456789abcdef".into();
    assert_eq!(
        resolve("m22", &registry, std::slice::from_ref(&seat))
            .unwrap()
            .transport,
        "n-0123456789abcdef"
    );
    assert!(matches!(
        resolve("m7", &registry, &[seat]),
        Err(LabelRefused::Absent(_))
    ));
    assert!(matches!(
        resolve("m22", &registry, &[]),
        Err(LabelRefused::Disputed(_))
    ));
}

#[test]
fn dispute_prints_both_registry_node_and_signed_seat_name() {
    let registry = registry();
    let mut seat = seat();
    seat.mac = Some("mc-0000000000000008".into());
    let Err(LabelRefused::Disputed(why)) = resolve("m7", &registry, &[seat]) else {
        panic!("a seat another Mac's facts bind is a dispute");
    };
    for evidence in [
        "0123456789abcdef",
        "log-name-seven",
        "mc-0000000000000007",
        "mc-0000000000000008",
    ] {
        assert!(why.contains(evidence), "missing evidence {evidence}: {why}");
    }
}

#[test]
fn conflicted_label_and_duplicate_adoption_refuse() {
    let mut registry = registry();
    assert!(matches!(
        resolve("m7", &registry, &[seat(), seat()]),
        Err(LabelRefused::Disputed(_))
    ));
    let mut collision = seat();
    collision.actor = [8; 48];
    collision.mac = Some("mc-0000000000000008".into());
    assert!(matches!(
        resolve("m7", &registry, &[seat(), collision]),
        Err(LabelRefused::Disputed(_))
    ));
    registry.rows.values_mut().next().unwrap().fields.insert(
        "label".into(),
        Field::Conflict {
            candidates: ["m7", "m8"]
                .into_iter()
                .map(|value| Candidate {
                    operation: [1; 32],
                    value: CellValue::owner(2, json!(value)),
                })
                .collect(),
            provisional: Some(json!("m7")),
        },
    );
    assert!(matches!(
        resolve("m7", &registry, &[seat()]),
        Err(LabelRefused::Conflicted(_))
    ));
}

#[test]
fn raw_node_diagnostics_never_print_untyped_values() {
    let mut registry = registry();
    let row = registry.rows.values_mut().next().unwrap();
    row.fields
        .insert("node".into(), field("untyped-private-fixture"));
    row.raw.insert("node".into());
    let Err(LabelRefused::Conflicted(why)) = resolve("m7", &registry, &[seat()]) else {
        panic!("a raw node cell is a missing binding");
    };
    assert!(!why.contains("untyped-private-fixture"));
    assert!(why.contains("log-name-seven"));
}

#[test]
fn typed_label_words_are_canonical_before_lookup() {
    let label = registry_label("M7-3FA2").expect("a typed clash label");
    let mut registry = registry();
    registry
        .rows
        .values_mut()
        .next()
        .unwrap()
        .fields
        .insert("label".into(), field(&label));
    assert_eq!(
        resolve(&label, &registry, &[seat()]).unwrap().transport,
        "00112233445566778899aabbccddeeff"
    );
    assert!(registry_label("m7.local").is_none());
}

/// A seat's machine is the one its enrollment's facts row says it is: a folded successor's
/// `merged_into`, not the born row its `mac` still binds, so its Mac answers its own label.
/// NEGATIVE CONTROL: with `seat_machine` reading `mac` alone, the seat names the born row:
/// `` left: Some("mc-1111111111111111") right: Some("mc-0000000000000007") ``.
#[test]
fn a_folded_enrollment_answers_the_machine_it_was_folded_into() {
    use aterm_db::typed::owned::{OwnedRead, OwnedRow};
    let uuid = "11111111111111111111111111111111";
    let mut facts = OwnedRead::default();
    facts.rows.insert(
        uuid.into(),
        OwnedRow {
            machine: [0; 16],
            actor: [0; 48],
            value: json!({"mac": "mc-1111111111111111", "merged_into": "mc-0000000000000007"}),
            rank: (0, 1, 1),
            operation: [0; 32],
        },
    );
    let mac = seat_machine(Some(&facts), uuid);
    assert_eq!(mac.as_deref(), Some("mc-0000000000000007"));
    let folded = LabelSeat { mac, ..seat() };
    assert_eq!(
        resolve("m7", &registry(), &[folded]).unwrap().transport,
        "00112233445566778899aabbccddeeff"
    );
    let row = facts.rows.get_mut(uuid).unwrap();
    row.value = json!({"mac": "mc-0000000000000007"});
    assert_eq!(
        seat_machine(Some(&facts), uuid).as_deref(),
        Some("mc-0000000000000007")
    );
    assert_eq!(
        seat_machine(Some(&facts), "22222222222222222222222222222222"),
        None
    );
    assert_eq!(seat_machine(None, uuid), None);
}

/// A second standing seat of [`seat`]'s Mac (another home on it, an enrollment not folded yet, or
/// the Mac back with a new key while its old seat stands), which holds another node.
fn second() -> LabelSeat {
    LabelSeat {
        actor: [9; 48],
        node: "fedcba9876543210".into(),
        name: "log-name-seven-again".into(),
        transport: "n-fedcba9876543210".into(),
        ..seat()
    }
}

/// A second standing seat of the Mac a row names holds another node: for a drive or a read, the
/// row's node cell selects the seat, in either order, and the other seat is no dispute.
/// NEGATIVE CONTROL: with the count of seats bound to the row's Mac held to one, as before, both
/// reads refuse with `registry and seat disagree`.
#[test]
fn a_row_two_standing_seats_bind_resolves_to_the_seat_that_holds_its_node() {
    for seats in [[seat(), second()], [second(), seat()]] {
        let held = resolve("m7", &registry(), &seats)
            .unwrap_or_else(|why| panic!("two seats of one Mac: {why}"));
        assert_eq!(held.actor, [7; 48]);
        assert_eq!(held.transport, "00112233445566778899aabbccddeeff");
    }
    // CONTROL: the row's node is the second seat's: that seat is the one it names.
    let mut registry = registry();
    registry
        .rows
        .values_mut()
        .next()
        .unwrap()
        .fields
        .insert("node".into(), field("fedcba9876543210"));
    assert_eq!(
        resolve("m7", &registry, &[seat(), second()]).unwrap().actor,
        [9; 48]
    );
}

/// Two live rows that carry one label name two Macs: refused, naming both rows, whatever the seats.
#[test]
fn two_live_rows_with_one_label_still_refuse() {
    let mut registry = registry();
    registry.rows.insert(
        "mc-0000000000000009".into(),
        Row {
            fields: [
                ("label".into(), field("m7")),
                ("node".into(), field("fedcba9876543210")),
            ]
            .into(),
            ..Row::default()
        },
    );
    let Err(LabelRefused::Disputed(why)) = resolve("m7", &registry, &[seat()]) else {
        panic!("two rows carry m7");
    };
    for evidence in [
        "mc-0000000000000007 node=0123456789abcdef",
        "mc-0000000000000009 node=fedcba9876543210",
        "2 live registry rows carry the label",
    ] {
        assert!(why.contains(evidence), "missing evidence {evidence}: {why}");
    }
}

/// A removal takes out one seat and the writer successors below it, so a label whose row's Mac
/// holds two standing seats names neither for a removal: refused, naming both, in either order,
/// while a drive or a read still takes the seat that holds the row's node. A Mac with one standing
/// seat is removed by its label, another Mac's seat beside it notwithstanding.
/// NEGATIVE CONTROL: with `LabelFor::Removal` read as `Reach`, the removal resolves to the seat
/// that holds the row's node: `a removal names neither of two seats`.
#[test]
fn a_removal_by_label_refuses_a_mac_that_holds_two_standing_seats() {
    for seats in [[seat(), second()], [second(), seat()]] {
        let Err(LabelRefused::Disputed(why)) =
            resolve_registry_label("m7", &registry(), Ok(&seats), LabelFor::Removal)
        else {
            panic!("a removal names neither of two seats");
        };
        for evidence in [
            "n-0123456789abcdef",
            "n-fedcba9876543210",
            "2 standing seats are the registry's Mac mc-0000000000000007",
            "remove each by its node id",
        ] {
            assert!(why.contains(evidence), "missing evidence {evidence}: {why}");
        }
        assert_eq!(
            resolve("m7", &registry(), &seats).unwrap().actor,
            [7; 48],
            "CONTROL: a drive or a read takes the seat that holds the node"
        );
    }
    let other_mac = LabelSeat {
        mac: Some("mc-0000000000000009".into()),
        ..second()
    };
    assert_eq!(
        resolve_registry_label(
            "m7",
            &registry(),
            Ok(&[seat(), other_mac]),
            LabelFor::Removal
        )
        .map(|seat| seat.actor)
        .ok(),
        Some([7; 48]),
        "CONTROL: the Mac's one standing seat is removed by its label"
    );
}

/// Where the seats are unread, the registry's rows are judged all the same, so an unread
/// membership never stands in for the registry's own answer: no live row (`Absent`) and two
/// (`Disputed`) refuse as such, and one row's node is carried with the refusal (`NoMembership`):
/// the one Mac the book may then name by the label.
/// NEGATIVE CONTROL: with the seats read before the rows, as before, `m8` reads as `NoMembership`
/// with no node.
#[test]
fn unread_seats_leave_the_registrys_own_answer_standing() {
    let unread: Result<&[LabelSeat], &str> = Err("m7: no acknowledged membership");
    let Err(LabelRefused::NoMembership { said, node }) =
        resolve_registry_label("m7", &registry(), unread, LabelFor::Reach)
    else {
        panic!("one live row names its node while the seats are unread");
    };
    assert_eq!(node.as_deref(), Some("0123456789abcdef"));
    for evidence in [
        "m7: no acknowledged membership",
        "n-0123456789abcdef",
        "mc-0000000000000007",
    ] {
        assert!(
            said.contains(evidence),
            "missing evidence {evidence}: {said}"
        );
    }
    assert!(matches!(
        resolve_registry_label("m8", &registry(), unread, LabelFor::Reach),
        Err(LabelRefused::Absent(_))
    ));
    let mut two = registry();
    two.rows.insert(
        "mc-0000000000000009".into(),
        Row {
            fields: [
                ("label".into(), field("m7")),
                ("node".into(), field("fedcba9876543210")),
            ]
            .into(),
            ..Row::default()
        },
    );
    let Err(LabelRefused::Disputed(why)) =
        resolve_registry_label("m7", &two, unread, LabelFor::Reach)
    else {
        panic!("two rows carry m7 whatever the seats");
    };
    assert!(why.contains("signed seats unread"), "{why}");
}
