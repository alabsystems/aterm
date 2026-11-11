// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_policy::engine::PolicyEngine;
use aterm_policy::profiles;

#[test]
fn standard_profile_wildcard_execute_does_not_overgrant_when_legacy_bool_is_false() {
    let auth = WindowMintAuthority::new();
    let engine = PolicyEngine::new(profiles::standard());

    assert!(
        auth.try_mint_with_engine(
            super::super::policy_gates::xtwinops_verdict(Some(&engine), 21),
            false
        )
        .is_none()
    );
}
