// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_wake_reads_back_as_itself_and_nothing_else_reads_as_one() {
    let wakes = [
        Wake::Table {
            key: TableKey {
                table: [0xab; 32],
                recovery: false,
            },
            length: 7,
        },
        Wake::Table {
            key: TableKey {
                table: [1; 32],
                recovery: true,
            },
            length: 0,
        },
        Wake::Members { length: 42 },
    ];
    for wake in wakes {
        assert_eq!(Wake::decode(wake.encode().as_bytes()), Some(wake));
    }
    for bad in [
        &b"stop\n"[..],
        b"atm-wake 2 members 1\n",
        b"atm-wake 1 members x\n",
        b"atm-wake 1 members 1",
        b"atm-wake 1 members 1 2\n",
        b"atm-wake 1 table AB 0 1\n",
    ] {
        assert_eq!(
            Wake::decode(bad),
            None,
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn cursors_read_back_and_a_damaged_line_is_skipped() {
    let mut cursors = Cursors::default();
    cursors.tables.insert(
        TableKey {
            table: [2; 32],
            recovery: false,
        },
        5,
    );
    cursors.tables.insert(
        TableKey {
            table: [2; 32],
            recovery: true,
        },
        1,
    );
    cursors.members = Some(9);
    let text = cursors.encode();
    assert_eq!(Cursors::decode(&text), cursors);
    let damaged = format!("{text}t zz p 1\nm\n");
    assert_eq!(Cursors::decode(&damaged), cursors);
}

/// The members cursor's generation and the record that names its place are a line of their
/// own ([`MembersCursor`]): a file with none (every file a build before FL-148D wrote) reads
/// as a log never rewritten with no record and is written back byte for byte, the `m` line
/// reads as it always has beside it, and a damaged one is skipped whole, one that names a
/// rewrite of a log never rewritten, or none of a rewritten one, among them.
#[test]
fn the_members_generation_is_a_line_of_its_own() {
    let mut cursors = Cursors {
        members: Some(9),
        ..Cursors::default()
    };
    let before = cursors.encode();
    assert_eq!(before, "m 9\n");
    assert_eq!(Cursors::decode(&before), cursors);

    // A log never rewritten, its cursor's place named.
    cursors.members_last = Some([0xcd; 32]);
    let text = cursors.encode();
    assert_eq!(text, format!("m 9\ng 0 - {}\n", "cd".repeat(32)));
    assert_eq!(Cursors::decode(&text), cursors);

    cursors.members_generation = Generation {
        number: 3,
        rewrite: Some([0xab; 32]),
    };
    let text = cursors.encode();
    assert_eq!(
        text,
        format!("m 9\ng 3 {} {}\n", "ab".repeat(32), "cd".repeat(32))
    );
    assert_eq!(Cursors::decode(&text), cursors);
    cursors.members_last = None;
    let text = cursors.encode();
    assert_eq!(text, format!("m 9\ng 3 {} -\n", "ab".repeat(32)));
    assert_eq!(Cursors::decode(&text), cursors);

    let unplaced = Cursors {
        members: Some(9),
        ..Cursors::default()
    };
    let (ab, cd) = ("ab".repeat(32), "cd".repeat(32));
    for damaged in [
        "m 9\ng\n".to_owned(),
        "m 9\ng x - -\n".to_owned(),
        "m 9\ng 3\n".to_owned(),
        format!("m 9\ng 3 {ab}\n"),
        format!("m 9\ng 3 zz {cd}\n"),
        format!("m 9\ng 3 {ab} zz\n"),
        format!("m 9\ng 3 {ab} {cd} 1\n"),
        format!("m 9\ng 0 {ab} {cd}\n"),
        format!("m 9\ng 3 - {cd}\n"),
    ] {
        assert_eq!(Cursors::decode(&damaged), unplaced, "{damaged:?}");
    }
}
