// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::bind_version;

#[test]
fn bind_version_caps_to_advertised_and_table_and_rejects_zero() {
    assert_eq!(bind_version(0, 6, 4), None);
    assert_eq!(bind_version(6, 0, 4), None);
    assert_eq!(bind_version(6, 6, 0), None);
    assert_eq!(bind_version(3, 6, 5), Some(3));
    assert_eq!(bind_version(8, 4, 6), Some(4));
    assert_eq!(bind_version(8, 6, 2), Some(2));
}
