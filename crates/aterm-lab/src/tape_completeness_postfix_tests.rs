// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn recording_and_cost_refuse_an_incomplete_inventory() {
    let owned = aterm_tempfile::tempdir().expect("exclusive fixture");
    let root = owned.path().join("tapes");
    std::fs::create_dir(&root).expect("owned directory");
    assert!(
        recording(&root)
            .expect("empty complete inventory")
            .is_empty()
    );
    assert_eq!(
        summary(&root).expect("empty complete count"),
        Cost::default()
    );
    let malformed = root.join("bad.atape");
    std::fs::write(&malformed, b"not a tape").expect("owned malformed tape");
    assert_eq!(summary(&root).expect("malformed is counted open").open, 1);
    let wrong = owned.path().join("wrong-type");
    std::fs::write(&wrong, b"occupied").expect("owned wrong type");
    let missing = owned.path().join("missing");
    for path in [&wrong, &missing] {
        assert!(
            recording(path).is_err() && summary(path).is_err(),
            "incomplete tape inventory is not a completed loop or partial cost"
        );
    }
    assert_eq!(
        std::fs::read(&wrong).expect("occupied retained"),
        b"occupied"
    );
}
