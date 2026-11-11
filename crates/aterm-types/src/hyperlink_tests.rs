// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn merge_scope_keeps_url_identity_and_id_value_distinct() {
    let url: Arc<str> = Arc::from("https://example.com");
    let first = Arc::new(HyperlinkData {
        url: url.clone(),
        id: Some(Arc::from("id")),
    });
    let equal_id = Arc::new(HyperlinkData {
        url: url.clone(),
        id: Some(Arc::from("id")),
    });
    let other_url = Arc::new(HyperlinkData {
        url: Arc::from(url.as_ref()),
        id: Some(Arc::from("id")),
    });
    let other_id = Arc::new(HyperlinkData {
        url,
        id: Some(Arc::from("other")),
    });
    assert!(HyperlinkData::same_scope(&first, &first));
    assert!(HyperlinkData::same_scope(&first, &equal_id));
    assert!(!HyperlinkData::same_scope(&first, &other_url));
    assert!(!HyperlinkData::same_scope(&first, &other_id));
}
