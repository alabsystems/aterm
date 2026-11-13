// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn capture_requires_one_driver_world_in_the_private_root() {
    let world = "WORLD isolated_source_proof=/private/tmp/.ubd/atsp42_abcdef_0.noindex\n";
    assert_eq!(
        world_from_output(world.as_bytes()).as_deref(),
        Some("/private/tmp/.ubd/atsp42_abcdef_0.noindex")
    );
    for bad in [
        "WORLD isolated_source_proof=/Users//person/.codex\n",
        "WORLD isolated_source_proof=/tmp/.ubd/atsp42.noindex/../elsewhere\n",
        "WORLD isolated_source_proof=relative/atsp42.noindex\n",
    ] {
        assert!(world_from_output(bad.as_bytes()).is_none());
    }
    assert!(world_from_output(format!("{world}{world}").as_bytes()).is_none());
}
