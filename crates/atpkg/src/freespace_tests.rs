// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn walks_up_to_existing_ancestor() {
    let d = std::env::temp_dir();
    let ghost = d.join("atpkg-freespace-does/not/exist");
    // A raw statvfs of a missing path fails; the walk-up resolves it to the existing
    // ancestor. Byte equality is not asserted: free space moves as other tests write.
    assert!(
        available_bytes(&d).is_some(),
        "the existing ancestor queries fine"
    );
    assert!(
        available_bytes(&ghost).is_some(),
        "a non-existent path resolves to its existing ancestor's volume"
    );
}
