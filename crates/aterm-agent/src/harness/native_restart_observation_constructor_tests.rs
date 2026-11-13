// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn passive_cold_witness_requires_an_advertised_restart_constructor() {
    assert!(observation_kind_enabled(&[Constructor::PrivateRestart]));
    assert!(observation_kind_enabled(&[Constructor::ColdRestart]));
    assert!(observation_kind_enabled(&[
        Constructor::OwnImage,
        Constructor::ColdRestart
    ]));
    assert!(!observation_kind_enabled(&[Constructor::OwnImage]));
    assert!(!observation_kind_enabled(&[]));
}
