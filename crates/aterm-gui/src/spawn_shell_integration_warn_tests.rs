// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ShellIntegrationOutcome, first_sighting};

#[test]
fn a_failed_outcome_is_new_once_per_process() {
    let seen = std::sync::Mutex::new(Vec::new());
    let sh = ShellIntegrationOutcome::UnknownShell("/bin/sh".to_string());
    assert!(first_sighting(&seen, &sh), "the first /bin/sh tab");
    for _ in 0..20 {
        assert!(!first_sighting(&seen, &sh), "every later /bin/sh tab");
    }
    assert!(first_sighting(
        &seen,
        &ShellIntegrationOutcome::UnknownShell("nu".to_string())
    ));
    let full = ShellIntegrationOutcome::WriteFailed("disk full".to_string());
    assert!(first_sighting(&seen, &full));
    assert!(!first_sighting(&seen, &full));
    assert!(first_sighting(
        &seen,
        &ShellIntegrationOutcome::WriteFailed("read-only".to_string())
    ));
}
