// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::home_relative_suffix;

#[test]
fn home_abbreviation_respects_component_boundaries() {
    for (path, home, want) in [
        ("/Users//foo", "/Users//foo", Some("")),
        ("/Users//foo/src/aterm", "/Users//foo", Some("/src/aterm")),
        ("/Users//foobar", "/Users//foo", None),
        ("/elsewhere/x", "/Users//foo", None),
        ("/anything", "", None),
        ("/anything", "/", None),
    ] {
        assert_eq!(
            home_relative_suffix(path, home),
            want,
            "{path} under {home:?}"
        );
    }
}
