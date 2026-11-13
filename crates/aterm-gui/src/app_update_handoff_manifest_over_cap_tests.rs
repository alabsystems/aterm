// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PreparationFailure, preparation_failure_of_write};
use crate::seamless::WriteOutgoingFailure;

#[test]
fn an_over_cap_manifest_is_filed_on_the_refusal_lane() {
    let refused = preparation_failure_of_write(WriteOutgoingFailure::ManifestOverCap);
    assert!(
        matches!(refused, PreparationFailure::Refused(_)),
        "a deterministic refusal of the desk's content"
    );
    for own in [
        WriteOutgoingFailure::NoPrivateDir,
        WriteOutgoingFailure::Inconsistent,
        WriteOutgoingFailure::Io(std::io::ErrorKind::StorageFull),
    ] {
        assert!(
            matches!(
                preparation_failure_of_write(own),
                PreparationFailure::Producer(_)
            ),
            "{own}: this process's own trouble"
        );
    }
    assert_eq!(
        crate::app_native::HandoffFailureLane::classify(
            crate::native_updater_service::ApplyMode::Automatic,
            crate::UpdateHandoffOutcome::CaptureRefused,
            crate::ChildDeathEvidence::Unobserved,
            false,
        ),
        crate::app_native::HandoffFailureLane::Refused
    );
}
