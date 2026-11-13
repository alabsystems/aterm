// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_invalid_or_unguarded_private_attachment_never_falls_back_to_the_lan() {
    for value in ["", "0", "65536", "private", "-1", "+1"] {
        assert!(parse(Some(OsStr::new(value)), Some(OsStr::new("1"))).is_err());
    }
    for guard in [None, Some(OsStr::new("0")), Some(OsStr::new(""))] {
        assert!(parse(Some(OsStr::new("49152")), guard).is_err());
    }
    assert_eq!(parse(None, None).unwrap(), None);
    assert_eq!(
        parse(Some(OsStr::new("49152")), Some(OsStr::new("1"))).unwrap(),
        Some(49152)
    );
}
