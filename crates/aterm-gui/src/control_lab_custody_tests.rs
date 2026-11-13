// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_the_local_unselected_instance_owner_can_receive_original_custody() {
    assert!(lab_custody_allowed(
        Scope::Owner,
        Principal::Instance,
        false,
        false,
        false
    ));
    assert!(!lab_custody_allowed(
        Scope::Owner,
        Principal::Harness,
        false,
        false,
        false
    ));
    for (selected, arguments, remote) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        assert!(!lab_custody_allowed(
            Scope::Owner,
            Principal::Instance,
            selected,
            arguments,
            remote
        ));
    }
    assert!(!lab_custody_allowed(
        Scope::Bridge,
        Principal::Instance,
        false,
        false,
        false
    ));
    for content in [false, true] {
        for drive in [false, true] {
            assert!(!lab_custody_allowed(
                Scope::Fleet { content, drive },
                Principal::Instance,
                false,
                false,
                false
            ));
        }
    }
}
