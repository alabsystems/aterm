// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared OSC 8 payloads carried by active cells and history spans.

use std::sync::Arc;

/// A hyperlink URL and its optional OSC 8 ID, shared as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkData {
    /// Hyperlink URL.
    pub url: Arc<str>,
    /// Explicit OSC 8 `id=` parameter, when supplied.
    pub id: Option<Arc<str>>,
}

impl HyperlinkData {
    /// Whether two payloads share the URL identity and ID value used to merge
    /// adjacent link ranges. Identical payloads need no string comparison.
    #[must_use]
    #[inline]
    pub fn same_scope(a: &Arc<Self>, b: &Arc<Self>) -> bool {
        Arc::ptr_eq(a, b) || (Arc::ptr_eq(&a.url, &b.url) && a.id == b.id)
    }
}

#[cfg(test)]
mod tests {
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
}
