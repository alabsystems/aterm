// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ABOUT PROVENANCE MODEL: the `(key, value)` build-provenance rows the native
//! Settings `/about` route paints, and the clipboard-ready block its COPY action puts on
//! the pasteboard.
//!
//! ONE source ([`crate::build_info::about_fields`]) feeds the route's pixels, its
//! semantics (`controls about` serializes that route's compiled semantic frame) and
//! [`provenance_text`], so no observer can disagree with another.
//!
//! **The own-rendered About CARD that used to live here was DELETED on 2026-09-15.** It
//! was an opaque floating [`crate::widget::DrawPrim`] panel with its own layout, painter,
//! accessibility tree, pointer hit-test and selectable-text model. Nothing had been able
//! to open it since the native whole-tab runtime landed (`72ac98091`, 2026-07-17): the
//! `Overlay::About` variant and `App::about_enter` were `cfg(test)`-only, and shipping
//! About became the native route. What was left was a retired surface still being
//! compiled, painted and regression-tested — this round's cruft. The MODEL below stays
//! because the native route reads it.

/// The build-provenance rows for the native Settings `/about` route.
pub(crate) struct AboutState {
    /// `(key, value)` provenance rows from `build_info::about_fields()`.
    rows: Vec<(&'static str, String)>,
}

/// The whole provenance block as clipboard-ready `key: value` lines — the SAME
/// [`crate::build_info::about_fields`] rows the route paints, so what lands on the
/// clipboard can never disagree with what is on screen. A free function of the build (no
/// per-window state), so the copy path needs no [`AboutState`] borrow.
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

    /// Structured read projection for the native Settings `/about` route: the route's
    /// rows, its semantics and the copy action all read these same provenance rows.
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

    /// The clipboard block and the route's rows come from the same source, so a build
    /// row can never appear on screen and be missing from a paste (or the reverse).
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

    /// The site row becomes an absolute URL whether or not the build spells a scheme,
    /// and a build with no site row offers no link.
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
