// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_inventory_error_is_reported_instead_of_looking_empty() {
    let owned = aterm_tempfile::tempdir().expect("owned scratch");
    let root = owned.path().join("tapes");
    std::fs::create_dir(&root).expect("tapes");
    assert!(replay_all(&root).is_empty(), "an ordinary empty inventory");
    let bad_tape = root.join("bad.atape");
    std::fs::write(&bad_tape, b"not a harness tape").expect("owned file");
    let ordinary = replay_all(&root);
    assert_eq!(ordinary.len(), 1, "the ordinary file is actually visited");
    assert_eq!(ordinary[0].0, bad_tape);
    assert!(ordinary[0].1.is_err(), "a malformed tape is reported");
    let wrong_type = owned.path().join("not-a-directory");
    std::fs::write(&wrong_type, b"occupied").expect("owned wrong type");
    let missing = owned.path().join("missing");
    let reported: Vec<bool> = [&wrong_type, &missing]
        .into_iter()
        .map(|dir| {
            let result = replay_all(dir);
            result.len() == 1 && result[0].0 == *dir && result[0].1.is_err()
        })
        .collect();
    assert!(
        reported.iter().all(|yes| *yes),
        "tape inventory errors must be reported, never an empty readable subset: {reported:?}"
    );
    assert_eq!(
        std::fs::read(&wrong_type).expect("still owned"),
        b"occupied"
    );
}
