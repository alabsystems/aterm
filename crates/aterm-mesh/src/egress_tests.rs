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
