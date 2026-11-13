// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use agh::error::{Kind, SideEffect};

#[test]
fn an_uncertain_native_write_is_never_a_lost_lease() {
    assert!(is_conflict(&agh::Error::new(Kind::Conflict, "changed")));
    for error in [
        agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Applied),
        agh::Error::new(Kind::OutcomeUnknown, "non-fast-forward; [rejected]")
            .with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::CouldNotRun, "fetch first"),
        agh::Error::new(Kind::Refused, "cannot lock ref 'refs/heads/main'"),
    ] {
        assert!(!is_conflict(&error));
    }
}

#[test]
fn only_a_settled_transport_failure_is_retryable() {
    assert!(is_transient(&agh::Error::could_not_run("HTTP 503")));
    for error in [
        agh::Error::could_not_run("HTTP 503").with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::Conflict, "stale info"),
        agh::Error::refused("protected"),
    ] {
        assert!(!is_transient(&error));
    }
}
