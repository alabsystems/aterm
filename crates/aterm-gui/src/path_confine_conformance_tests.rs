// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the control-socket image-path confinement to
//! `aterm_models_gui::path_confine_model` (`PathConfine`).
//!
//! Drives the real `control_auth::confine_image_path` and the retained-handle writer over
//! an honest request and a final-component symlink pointing outside the root, projects
//! each onto `<<decided, committed, target>>` (target 0 none, 1 inside, 2 outside) in the
//! configuration the request is (`LinkOutside`), and checks each `Confine` step on the
//! model. The negative control, an escape that commits outside, is refused, and admitted
//! only by the mutant that follows the link. Unix only: the escape half plants a POSIX
//! symlink; Windows has its own reparse-point and handle-identity coverage.
#![cfg(unix)]

use std::collections::BTreeMap;

use aterm_models_gui::path_confine_model;
use aterm_spec::verify::validate_transition_tiered;

use crate::control_auth::confine_image_path;

fn state(decided: i64, committed: i64, target: i64) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("decided", decided),
        ("committed", committed),
        ("target", target),
    ])
}

/// Whether the model admits `Init -> (committed, target)` by `Confine` for a request
/// whose final component does (`link_outside`) or does not resolve outside the root.
fn confine(link_outside: bool, committed: i64, target: i64, buggy: i64) -> (bool, String) {
    validate_transition_tiered(
        &path_confine_model(),
        &[("LinkOutside", i64::from(link_outside)), ("Buggy", buggy)],
        &state(0, 0, 0),
        &state(1, committed, target),
        Some("Confine"),
        "PathConfine",
    )
}

#[test]
fn real_path_confine_conforms_to_the_pathconfine_model() {
    let scratch = aterm_tempfile::TempDir::new().expect("scratch socket dir");
    let dir = scratch.path();

    // HONEST request: confine returns Some and the writer commits inside the root.
    let confined = confine_image_path(dir, "shot.png").expect("honest request must confine");
    let written = confined
        .write_private(b"\x89PNG\r\n\x1a\nstub")
        .expect("write inside the root must succeed");
    confined
        .validate_for_reply(&written)
        .expect("the confined path identity remains current");
    let canon_images = std::fs::canonicalize(dir.join("images")).expect("canon images");
    assert!(
        confined.display_path().starts_with(&canon_images),
        "committed path {:?} is inside the canonical images root {canon_images:?}",
        confined.display_path(),
    );
    let (ok, why) = confine(false, 1, 1, 0);
    assert!(
        ok,
        "the real honest Confine (committed inside) must conform: {why}"
    );

    // ESCAPE request: a final-component symlink images/evil.png -> a file outside the
    // root. confine rejects it, so nothing is committed and the victim is intact.
    let images = confined.dir.clone();
    let victim = dir.join("victim_outside.txt");
    std::fs::write(&victim, b"original").expect("plant the victim");
    std::os::unix::fs::symlink(&victim, images.join("evil.png")).expect("plant the escape");
    assert!(
        confine_image_path(dir, "evil.png").is_none(),
        "a final-component symlink escape MUST be rejected by confine"
    );
    assert_eq!(
        std::fs::read(&victim).expect("read the victim"),
        b"original",
        "the victim outside the root was NOT clobbered"
    );
    let (ok, why) = confine(true, 0, 0, 0);
    assert!(
        ok,
        "the real escape Confine (rejected, no write) must conform: {why}"
    );

    // Negative control: an escape request committing outside (a writer that follows the
    // symlink). Refused by the committed model; the link-following mutant admits it.
    let (ok, _) = confine(true, 1, 2, 0);
    assert!(
        !ok,
        "an escape that commits OUTSIDE the root MUST be refused"
    );
    let (ok, why) = confine(true, 1, 2, 1);
    assert!(ok, "the link-following mutant admits it: {why}");
}
