// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn range_changes_share_but_value_edits_detach() {
    let original = HyperlinkSpan::with_id(
        0,
        80,
        Arc::from("https://example.com"),
        Some(Arc::from("original")),
    );
    let mut clipped = original.with_range(2, 4);
    assert!(Arc::ptr_eq(&original.payload, &clipped.payload));
    clipped.start_col = 1;
    assert_eq!((original.start_col, original.end_col), (0, 80));
    assert!(Arc::ptr_eq(&original.payload, &clipped.payload));
    clipped.id = Some(Arc::from("edited"));
    assert_eq!(original.id.as_deref(), Some("original"));
    assert_eq!(clipped.id.as_deref(), Some("edited"));
    assert!(!Arc::ptr_eq(&original.payload, &clipped.payload));
    assert!(Arc::ptr_eq(&original.url, &clipped.url));
    drop(original);
    assert_eq!(clipped.url.as_ref(), "https://example.com");
}
