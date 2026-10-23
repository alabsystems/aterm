// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the control socket's image-path confinement: it holds for an honest request
//! and for a planted escape link, and each law is broken by the mutant that follows the
//! link.

use aterm_models_gui::path_confine_model;
use aterm_spec::interp;

#[test]
fn an_image_write_lands_inside_the_root_and_an_escape_commits_nothing() {
    let m = path_confine_model();
    let escape = interp::with_consts(&m, &[("LinkOutside", 1)]);
    // With no link planted the mutant has nothing to follow, so the escape configuration
    // is the one its mutant breaks.
    let broken: Vec<&str> = interp::violated_invariants(&interp::with_buggy(&escape, 1))
        .into_iter()
        .collect();
    assert_eq!(broken, ["EscapeRejected", "WriteWithinSubdir"]);
    assert!(interp::violated_invariants(&interp::with_buggy(&m, 1)).is_empty());
}
