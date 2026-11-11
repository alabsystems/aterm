// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn decscl_param_roundtrip() {
    for level in [
        VtLevel::VT100,
        VtLevel::VT220,
        VtLevel::VT320,
        VtLevel::VT420,
        VtLevel::VT520,
    ] {
        let param = level.decscl_param();
        let recovered = VtLevel::from_decscl_param(param);
        assert_eq!(recovered, Some(level), "Roundtrip failed for {level}");
    }
}
