// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn fifo_tickets_survive_rotation_gaps_and_prune_ineligible_members() {
    let mut row =
        json!({"key":"00".repeat(32),"provider":"google","serial":0,"tickets":[],"grants":[]});
    let valid = ["m1", "m2", "m3", "m4", "m5"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let actors = ["a4", "a5"].into_iter().map(str::to_owned).collect();
    let candidates = vec![("a4".into(), "m4".into()), ("a5".into(), "m5".into())];
    let warm = vec!["m1".into(), "m2".into(), "m3".into()];
    assert!(
        choose(&mut row, &warm, &candidates, &valid, &actors, false)
            .unwrap()
            .is_none()
    );
    let saved = row["tickets"].clone();
    assert!(
        choose(&mut row, &warm, &[], &valid, &actors, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(row["tickets"], saved);
    let next = choose(&mut row, &warm, &candidates, &valid, &actors, true)
        .unwrap()
        .unwrap();
    assert_eq!(next, vec!["m2", "m3", "m4"]);
    let next = choose(&mut row, &next, &candidates, &valid, &actors, true)
        .unwrap()
        .unwrap();
    assert_eq!(next, vec!["m3", "m4", "m5"]);
    let mut valid = valid;
    valid.remove("m3");
    assert_eq!(
        choose(&mut row, &next, &[], &valid, &actors, true)
            .unwrap()
            .unwrap(),
        vec!["m4", "m5"]
    );
}
