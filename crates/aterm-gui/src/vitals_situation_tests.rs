// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A reader that answered reads its value; one that has not reads unread; one this OS lacks
/// reads unsupported, never a value or a zero. The displays' unread state leaves docked
/// unread, not undocked.
/// NEGATIVE CONTROL: `reading` mapping `(true, None)` to `Unsupported` fails the second
/// assertion.
#[test]
fn an_unanswered_reader_reads_unread_and_a_missing_one_unsupported() {
    assert_eq!(reading(true, Some(3u8)), Reading::Read(3));
    assert_eq!(reading::<u8>(true, None), Reading::Unread);
    assert_eq!(reading(false, Some(3u8)), Reading::Unsupported);
    let s = situation(
        Reading::Unread,
        None,
        probe::Heard {
            lid: Some(probe::LidState {
                lid: probe::Lid::Open,
                changed_ms: None,
            }),
            external_displays: None,
            awake: None,
        },
    );
    assert_eq!(
        s.lid.read().map(|l| l.lid),
        probe::LID_READER.then_some(Lid::Open)
    );
    assert_ne!(
        s.docked(),
        Reading::Read(Some(false)),
        "never undocked unread"
    );
    assert_eq!(
        s.power.why(),
        Some(if probe::POWER_READER {
            "not_sampled"
        } else {
            "unsupported"
        })
    );
}

/// A unit test's reader reads nothing: no value of its situation is a reading.
#[test]
fn a_unit_tests_reader_reads_nothing() {
    let s = SituationReader::inert().read(0);
    assert_eq!(s.machine.read(), None);
    assert_eq!(s.lid.read(), None);
    assert_eq!(s.power.read(), None);
    assert_eq!(s.displays.read(), None);
    assert_eq!(s.awake.read(), None);
}
