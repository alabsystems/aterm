// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Admission of an immutable browser route against a root-approved profile
/// requirement. This models selection, not browser or human attestation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_browser_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedBrowser {
            const Buggy = 0;
            var case = 0; var required = 0; var matching = 0; var accepted = 0;
            action OptionalDefault when (case == 0) { case = 1; accepted = if Buggy == 5 {0} else {1}; }
            action RequiredDefault when (case == 0) { case = 2; required = 1; accepted = if Buggy == 1 {1} else {0}; }
            action OptionalRemote when (case == 0) { case = 3; accepted = if Buggy == 5 {0} else {1}; }
            action RequiredRemote when (case == 0) { case = 4; required = 1; accepted = if Buggy == 2 {1} else {0}; }
            action OptionalProfile when (case == 0) { case = 5; accepted = if Buggy == 5 {0} else {1}; }
            action WrongProfile when (case == 0) { case = 6; required = 1; accepted = if Buggy == 3 {1} else {0}; }
            action RequiredProfile when (case == 0) { case = 7; required = 1; matching = 1; accepted = if Buggy == 4 {0} else {1}; }
            invariant EnforceRequired: required == 0 || matching == 1 || accepted == 0;
            invariant AllowSupported: case == 0 || (required == 1 && matching == 0) || accepted == 1;
        }
    }
}

/// Bounded shared-renewal outcomes with a root-known provider identity, replay
/// and metadata rollback. Native conformance runs the guarded renewal itself.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_renewal_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedRenewal {
            const Buggy = 0;
            var case = 0; var entered = 0; var bound = 0;
            var pinned = 0; var consumed = 0; var ever = 0;
            var replayed = 0; var after = 0; var edited = 0; var restarted = 0;
            var homes = 0; var after_homes = 0; var reserved = 0;
            action Good when (case == 0) { case = 1; entered = 1; bound = if Buggy == 1 {0} else {1}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action StoppedBefore when (case == 0) { case = 2; entered = if Buggy == 2 {1} else {0}; bound = if Buggy == 2 {1} else {0}; }
            action StoppedDuring when (case == 0) { case = 3; entered = 1; bound = if Buggy == 3 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = if Buggy == 13 {0} else {1}; }
            action WrongPrincipal when (case == 0) { case = 4; entered = 1; bound = if Buggy == 4 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action ChangedRow when (case == 0) { case = 5; entered = 1; bound = if Buggy == 5 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action Crash when (case == 0) { case = 6; entered = 1; bound = if Buggy == 6 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action ChangedHead when (case == 0) { case = 7; entered = 1; bound = if Buggy == 7 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action MissingPrincipal when (case == 0) { case = 8; entered = 1; bound = if Buggy == 8 {1} else {0}; pinned = 1; consumed = 1; ever = 1; homes = 1; reserved = 1; }
            action Replay when (case > 0 && replayed == 0) { replayed = 1; after = if Buggy == 9 {entered + 1} else {entered}; after_homes = if Buggy == 12 {homes + 1} else {homes}; }
            action MetadataEdit when (pinned == 1 && edited == 0) { edited = 1; pinned = if Buggy == 10 {0} else {pinned}; }
            action Restart when (pinned == 1 && restarted == 0) { restarted = 1; consumed = if Buggy == 11 {0} else {consumed}; }
            invariant EligibleProgress: case == 0 || case > 1 || bound == 1;
            invariant OnlyProvedCurrentLogin: bound == 0 || case == 1;
            invariant NoStoppedEntry: case <= 1 || case > 2 || entered == 0;
            invariant NoReplay: replayed == 0 || after == entered;
            invariant NoReplayStaging: replayed == 0 || after_homes == homes;
            invariant RetainPin: consumed <= pinned;
            invariant NoRefund: ever <= consumed;
            invariant ApprovedPrincipalRetained: consumed <= reserved;
        }
    }
}
