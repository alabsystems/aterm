// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::agents_auto_prime_from_text;

#[test]
fn the_knob_reads_without_a_full_config_load() {
    assert!(agents_auto_prime_from_text(""), "empty file: default");
    assert!(
        agents_auto_prime_from_text("tab_status = false"),
        "unrelated key: default"
    );
    assert!(!agents_auto_prime_from_text("agents_auto_prime = false"));
    assert!(agents_auto_prime_from_text("agents_auto_prime = true"));
    assert!(agents_auto_prime_from_text("agents_auto_prime = "));
    assert!(agents_auto_prime_from_text("agents_auto_prime = \"no\""));
}
