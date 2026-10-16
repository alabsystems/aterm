// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build-provenance rows the native Settings `/about` route paints, and the
//! clipboard block its Copy action writes. One source
//! ([`crate::build_info::about_fields`]) feeds the route's pixels, its semantics
//! (`controls about`) and [`provenance_text`], so no observer can disagree with another.

/// The build-provenance rows for the native Settings `/about` route.
pub(crate) struct AboutState {
    /// `(key, value)` provenance rows from `build_info::about_fields()`.
    rows: Vec<(&'static str, String)>,
}

/// The whole provenance block as clipboard-ready `key: value` lines, from the same
/// [`crate::build_info::about_fields`] rows the route paints. A function of the build,
/// so the copy path needs no [`AboutState`].
#[must_use]
pub(crate) fn provenance_text() -> String {
    let mut out = String::from("aterm\n");
    for (k, v) in crate::build_info::about_fields() {
        out.push_str(&format!("{k}: {v}\n"));
    }
    out
}

/// The byline link's target: the `site` provenance row as an absolute https URL
/// (`None` if the build carries no site row).
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

    /// Every row the route paints is in the clipboard block.
    #[test]
    fn provenance_text_carries_every_row_the_route_reads() {
        let state = AboutState::new();
        let text = provenance_text();
        assert!(text.starts_with("aterm\n"));
        for (key, value) in state.semantic_rows() {
            assert!(
                text.contains(&format!("{key}: {value}\n")),
                "the clipboard block is missing the {key} row the route paints"
            );
        }
    }

    /// The site row becomes an absolute URL with or without a scheme; no row, no link.
    #[test]
    fn site_url_is_absolute_or_absent() {
        let mut state = AboutState::new();
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
