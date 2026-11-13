// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_agent::supervise::{Occupancy, SupervisorConfig};

use super::*;

#[test]
fn a_harness_key_lands_under_the_root_harness_table() {
    let key = vec!["occupancy = \"prompt\"".to_string()];
    let config = config_after(&config_with(&[]), true, &[]);
    let keyed = with_harness_keys(&config, &key);
    assert!(
        keyed.contains("[harness]\nenabled = true\noccupancy = \"prompt\"\n"),
        "{keyed}"
    );
    let (cfg, notes) = SupervisorConfig::from_aterm_toml(&keyed);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(cfg.occupancy, Occupancy::Prompt, "{keyed}");
    assert!(cfg.enabled);
    for unkeyed in [
        with_harness_keys(&config, &[]),
        config_after(&config_with(&[]), true, &key),
    ] {
        let (cfg, _) = SupervisorConfig::from_aterm_toml(&unkeyed);
        assert_ne!(cfg.occupancy, Occupancy::Prompt, "{unkeyed}");
    }
    assert_eq!(with_harness_keys("[update]\n", &key), "[update]\n");
}
