// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-provenance rows for the native Settings `/about` route and its Copy action,
//! sourced from [`crate::build_info::about_fields`].

/// The build-provenance rows for the native Settings `/about` route.
pub(crate) struct AboutState {
    /// `(key, value)` provenance rows from `build_info::about_fields()`.
    rows: Vec<(&'static str, String)>,
}

/// The provenance block as clipboard-ready `key: value` lines.
#[must_use]
pub(crate) fn provenance_text() -> String {
    let mut out = String::from("aterm\n");
    for (k, v) in crate::build_info::about_fields() {
        out.push_str(&format!("{k}: {v}\n"));
    }
    out
}

/// The `site` provenance row as an absolute URL (`https://` when it names no scheme), if present.
pub(crate) fn site_url(state: &AboutState) -> Option<String> {
    let (_, v) = state.rows.iter().find(|(k, _)| *k == "site")?;
    Some(if v.contains("://") {
        v.clone()
    } else {
        format!("https://{v}")
    })
}

impl AboutState {
    pub(crate) fn new() -> Self {
        Self {
            rows: crate::build_info::about_fields(),
        }
    }

    /// The provenance rows the route paints, its semantics and the copy action read.
    pub(crate) fn semantic_rows(&self) -> &[(&'static str, String)] {
        &self.rows
    }

    /// Test seam: replace the shipping `site` row (`alab.systems`) with a
    /// caller-chosen one, so link tests pin behaviour rather than the address.
    #[cfg(test)]
    pub(crate) fn add_test_site(&mut self, site: &str) {
        self.rows.retain(|(key, _)| *key != "site");
        self.rows.push(("site", site.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_url_is_absolute_or_absent() {
        let mut state = AboutState {
            rows: vec![("version", "0.0.0".to_string())],
        };
        state.add_test_site("example.test");
        assert_eq!(site_url(&state).as_deref(), Some("https://example.test"));
        state.add_test_site("https://example.test/path");
        assert_eq!(
            site_url(&state).as_deref(),
            Some("https://example.test/path")
        );
        state.rows.retain(|(key, _)| *key != "site");
        assert_eq!(site_url(&state), None);
    }
}
