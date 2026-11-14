// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::says_fleet;

/// ONLY THE INSTANCE'S OWN WORD SAYS ANOTHER MAC IS BEHIND A SESSION: `fleet=yes` as a
/// field of the answer. An answer without the field (an instance from before it), one that
/// says no, and a value that merely holds the letters say nothing.
#[test]
fn only_the_instances_own_word_says_another_mac_is_behind_the_session() {
    let lines = |text: &str| vec![text.to_owned()];
    assert!(says_fleet(&lines("verdict=driven session=s-1a fleet=yes")));
    assert!(says_fleet(&lines("verdict=remote session=- fleet=yes")));
    assert!(!says_fleet(&lines("verdict=driven session=s-1a fleet=no")));
    assert!(!says_fleet(&lines("verdict=allowed session=s-1a")));
    assert!(!says_fleet(&lines("verdict=allowed session=xfleet=yes")));
    assert!(!says_fleet(&[]));
}
