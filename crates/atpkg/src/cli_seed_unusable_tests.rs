// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Narrowing, seed_unusable_is_informational};

#[test]
fn an_unserved_architecture_and_a_removal_are_facts_an_exclusion_is_not() {
    let unserved =
        super::unserved_sentence("x86_64-apple-darwin", &["aarch64-apple-darwin".into()]);
    assert_eq!(
        unserved,
        "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
         — it builds for aarch64-apple-darwin; nothing installs here until one is published"
    );
    assert!(seed_unusable_is_informational(&unserved), "{unserved}");
    let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let (removed, _) = Narrowing {
        excluded: Vec::new(),
        removed: names(&["ay", "trust"]),
    }
    .verdict("aarch64-apple-darwin");
    assert_eq!(
        removed,
        "every ALab program available here was removed on this machine (ay, trust) — \
         nothing to install. `aterm pkg install <program>` brings one back"
    );
    assert!(seed_unusable_is_informational(&removed), "{removed}");
    let (excluded, _) = Narrowing {
        excluded: names(&["ay", "clean"]),
        removed: Vec::new(),
    }
    .verdict("aarch64-apple-darwin");
    assert_eq!(
        excluded,
        "[packages].exclude narrows the toolset to nothing installable on this machine \
         (aarch64-apple-darwin; excluded: ay, clean) — shorten it to receive the toolset"
    );
    assert!(!seed_unusable_is_informational(&excluded), "{excluded}");
    let (both, _) = Narrowing {
        excluded: names(&["ay"]),
        removed: names(&["trust"]),
    }
    .verdict("aarch64-apple-darwin");
    assert!(both.contains("excluded: ay; uninstalled: trust"), "{both}");
    assert!(!seed_unusable_is_informational(&both), "{both}");
}
