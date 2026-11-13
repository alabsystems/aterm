// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn message(kind: u16, seq: u32) -> Vec<u8> {
    let mut frame = vec![0; if kind == 3 { 20 } else { 16 }];
    let len = u32::try_from(frame.len()).unwrap();
    frame[..4].copy_from_slice(&len.to_ne_bytes());
    frame[4..6].copy_from_slice(&kind.to_ne_bytes());
    frame[8..12].copy_from_slice(&seq.to_ne_bytes());
    frame
}

#[test]
fn complete_dump_has_no_datagram_count_ceiling() {
    let now = Instant::now();
    let mut reads = 0;
    let rows = collect_dump(
        now - Duration::from_secs(1),
        DUMP_HANG,
        || now,
        |room, end| {
            assert_eq!(end, now + Duration::from_secs(59));
            assert!(reads <= 4096, "a completed dump must stop receiving");
            let frame = message(if reads < 4096 { 1 } else { 3 }, SEQ);
            reads += 1;
            room[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        },
    )
    .unwrap();
    assert_eq!(reads, 4097);
    assert!(rows.is_empty());
}

#[test]
fn expired_and_wrong_sequence_dumps_remain_refused() {
    let now = Instant::now();
    let error = collect_dump(
        now - Duration::from_secs(61),
        DUMP_HANG,
        || now,
        |_, _| panic!("an expired dump must not receive"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let frame = message(3, SEQ + 1);
    let error = collect_dump(
        now,
        DUMP_HANG,
        || now,
        |room, _| {
            room[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn each_receive_keeps_its_caller_bound_and_original_dump_cap() {
    let now = Instant::now();
    for (age, room_left) in [(1, 2), (59, 1)] {
        let rows = collect_dump(
            now - Duration::from_secs(age),
            Duration::from_secs(2),
            || now,
            |room, end| {
                assert_eq!(end, now + Duration::from_secs(room_left));
                let frame = message(3, SEQ);
                room[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            },
        )
        .unwrap();
        assert!(rows.is_empty());
    }
}
