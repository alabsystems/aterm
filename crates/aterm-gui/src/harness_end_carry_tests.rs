// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The carried end's row (`SessionRecord::autopilot_end`): what the park
//! writes and what the successor reads back.

use super::*;

/// AN END AND ITS WORK READ BACK AS WRITTEN, and nothing is no row: an
/// unseen landing, a seen one, work with no end; a session with neither
/// writes no key. NEGATIVE CONTROLS: each row that must not read — no `end=`,
/// no kind, a kind that is no end (`done` among them: Autopilot Land has no
/// other end), no time or one at the epoch, no `seen=`, a `seen` that is not
/// 0 or 1, a count that is no count, a key twice, a field with no `=` —
/// carries nothing; a key this build does not know is skipped, so a later
/// build's row still reads.
#[test]
fn an_end_and_its_work_read_back_as_written() {
    let at = 1_760_978_520_000_i64;
    let carries = [
        EndCarry {
            end: Some(CarriedEnd {
                face: EndFace::Landed,
                at_ms: at,
                seen: false,
            }),
            worked_ms: 25_000,
        },
        EndCarry {
            end: Some(CarriedEnd {
                face: EndFace::Landed,
                at_ms: at,
                seen: true,
            }),
            worked_ms: 0,
        },
        EndCarry {
            end: None,
            worked_ms: 25_000,
        },
    ];
    let rows: Vec<String> = carries
        .iter()
        .map(|carry| carry.row().expect("a row"))
        .collect();
    assert_eq!(
        rows,
        [
            "end=landed@1760978520000 seen=0 worked=25000",
            "end=landed@1760978520000 seen=1 worked=0",
            "end=- worked=25000",
        ]
    );
    for (carry, row) in carries.iter().zip(&rows) {
        assert_eq!(EndCarry::parse(row).as_ref(), Some(carry), "{row}");
    }
    assert_eq!(EndCarry::default().row(), None, "nothing: no key");
    assert_eq!(
        EndCarry::parse("end=landed@1760978520000 seen=0 worked=25000 cheered=1"),
        Some(carries[0]),
        "a later build's key is skipped"
    );
    assert_eq!(
        EndCarry::parse("end=landed@1760978520000 seen=1"),
        Some(carries[1]),
        "no work said: none"
    );
    // What the successor's timeline holds for the loop's restate: the end's
    // own face and time.
    assert_eq!(
        carries[0].end.map(CarriedEnd::told),
        Some((TaskFace::Landed, at))
    );
    for unreadable in [
        "",
        "worked=25000",
        "end= worked=1",
        "end=landed seen=0",
        "end=done@1760978520000 seen=0",
        "end=open@1760978520000 seen=0",
        "end=theirs@1760978520000 seen=0",
        "end=landed@0 seen=0",
        "end=landed@-5 seen=0",
        "end=landed@soon seen=0",
        "end=landed@1760978520000",
        "end=landed@1760978520000 seen=2",
        "end=landed@1760978520000 seen=0 worked=-1",
        "end=landed@1760978520000 seen=0 worked=lots",
        "end=- end=landed@1760978520000 seen=0",
        "end=landed@1760978520000 seen=0 seen=1",
        "end=- worked",
    ] {
        assert_eq!(EndCarry::parse(unreadable), None, "{unreadable:?}");
    }
}
