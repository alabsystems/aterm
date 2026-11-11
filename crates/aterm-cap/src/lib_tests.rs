// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use effects::Spawn;

// Test helper: mint the root authority. SAFETY: a test process with no untrusted input
// satisfies the trusted-launcher contract of `root_authority`.
fn test_authority() -> Authority {
    unsafe { Authority::root_authority() }
}

#[test]
fn granted_cap_carries_its_tier() {
    assert_eq!(Tier::Untrusted.rank(), 0);
    assert_eq!(Tier::Trusted.rank(), 1);
    assert_eq!(Tier::Certified.rank(), 2);

    let authority = test_authority();
    let spawn: Cap<Spawn> = authority.grant(Tier::Trusted);
    assert_eq!(spawn.tier(), Tier::Trusted);
    assert!(spawn.satisfies(Tier::Untrusted));
    assert!(spawn.satisfies(Tier::Trusted));
    assert!(!spawn.satisfies(Tier::Certified));

    let tiers = [Tier::Untrusted, Tier::Trusted, Tier::Certified];
    for granted in tiers {
        let cap: Cap<Spawn> = authority.grant(granted);
        assert_eq!(cap.tier(), granted);
        for min in tiers {
            assert_eq!(cap.satisfies(min), granted.rank() >= min.rank());
            assert_eq!(cap.satisfies(min), granted >= min);
        }
    }
}

#[test]
fn require_gates_on_tier() {
    let authority = test_authority();
    let clip: Cap<Spawn> = authority.grant(Tier::Untrusted);
    assert_eq!(
        require(&clip, Tier::Trusted),
        Err(Denied {
            have: Tier::Untrusted,
            need: Tier::Trusted
        })
    );
    assert_eq!(require(&clip, Tier::Untrusted), Ok(()));

    let certified: Cap<Spawn> = authority.grant(Tier::Certified);
    assert_eq!(require(&certified, Tier::Trusted), Ok(()));
}
