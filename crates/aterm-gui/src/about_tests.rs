// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn site_url_is_absolute_or_absent() {
    let mut state = AboutState {
        rows: std::cell::OnceCell::from(vec![("version", "0.0.0".to_string())]),
    };
    state.add_test_site("example.test");
    assert_eq!(site_url(&state).as_deref(), Some("https://example.test"));
    state.add_test_site("https://example.test/path");
    assert_eq!(
        site_url(&state).as_deref(),
        Some("https://example.test/path")
    );
    state
        .rows
        .get_mut()
        .expect("set")
        .retain(|(key, _)| *key != "site");
    assert_eq!(site_url(&state), None);
}
