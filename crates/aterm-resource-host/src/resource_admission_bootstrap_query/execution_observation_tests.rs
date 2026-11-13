// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn failed_birth_observation_never_becomes_a_retained_native_identity() {
    let pid = std::process::id();
    let mut identity = control::Identity {
        pid,
        birth: 0,
        birth_kind: if cfg!(target_os = "macos") {
            control::BirthKind::MacosWallMicroseconds
        } else {
            control::BirthKind::LinuxStartTicks
        },
    };
    // This is the exact placeholder retained by accept_launch if the
    // native birth read fails. A known PID cannot supply the missing birth.
    assert!(NativeObservation::known_identity(identity).is_none());

    identity.birth = process::read_birth(pid as i32)
        .expect("actual current process birth")
        .0;
    assert_ne!(identity.birth, 0);
    assert_eq!(NativeObservation::known_identity(identity), Some(identity));

    // Losing the observation again cannot be filled from the known PID or
    // an earlier successful observation of that process.
    identity.birth = 0;
    assert!(NativeObservation::known_identity(identity).is_none());
}
