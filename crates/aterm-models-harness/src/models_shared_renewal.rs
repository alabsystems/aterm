// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An admitted shared attempt fails around checkpoint creation, recovers if its
/// evidence completed, and requires a newer grant for another provider entry.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_checkpoint_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedCheckpoint {
            const Buggy = 0;
            var prior = 0; var fault = 0; var phase = 0;
            var entered = 0; var consumed = 0; var pending = 0;
            var published = 0; var bound = 0; var kept = 1; var once = 1; var same_refused = 0;
            action PriorBinding when (phase == 0 && prior == 0) { prior = 1; bound = 1; }
            action AfterCheckpoint when (phase == 0 && fault == 0) { fault = 1; }
            action Fail when (phase == 0) {
                phase = 1; entered = 1;
                consumed = if Buggy == 1 {0} else {1};
                pending = if Buggy == 2 {0} else {1};
                same_refused = if Buggy == 9 {0} else {1};
                kept = if Buggy == 7 {0} else {1};
            }
            action Recover when (phase == 1) {
                phase = 2;
                entered = if Buggy == 6 {2} else {entered};
                published = if Buggy == 4 {1 - fault} else {fault};
                bound = if fault == 1 {if Buggy == 3 {0} else {1}} else {prior};
                pending = 1 - fault;
                once = if Buggy == 5 {0} else {1};
            }
            action Supersede when (phase == 2) {
                phase = 3; entered = if Buggy == 8 {1} else {2};
                consumed = 2; pending = 0; published = 1; bound = 1;
            }
            invariant Consumed: phase == 0 || consumed == if phase == 3 {2} else {1};
            invariant FailedParent: phase == 0 || phase > 1 || (bound == prior && pending == 1 && published == 0);
            invariant ExactRecovery: phase <= 1 || phase > 2 || (published == fault && pending == 1 - fault && bound == if fault == 1 {1} else {prior});
            invariant Idempotence: once == 1;
            invariant NoReplay: phase == 0 || phase == 3 || (entered == 1 && same_refused == 1);
            invariant PreserveEvidence: kept == 1;
            invariant NewGrantProgress: phase <= 2 || (entered == 2 && bound == 1 && published == 1 && pending == 0);
        }
    }
}

/// The returned renewal parent is the committed image, with no interleaved writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn renewal_snapshot_model() -> Model {
    aterm_spec::ty_model! {
        AccountRenewalSnapshot {
            const Buggy = 0;
            var shared = 0; var case = 1; var phase = 0;
            var returned = 0; var exact = 1; var excluded = 0;
            var released = 1; var stale = 1; var consumed = 0; var token = 0;
            action AnotherMode when (phase == 0 && shared == 0) { shared = 1; }
            action AnotherCase when (phase == 0 && case <= 3) { case = case + 1; }
            action Begin when (phase == 0) {
                phase = 1;
                returned = if case == 4 {if Buggy == 7 {1} else {0}}
                    else {if Buggy == 8 {0} else {1}};
                exact = if Buggy == 1 {0} else {1};
                excluded = if Buggy == 2 {0} else {1};
                released = if Buggy == 3 {0} else {1};
                stale = if Buggy == 4 {0} else {1};
                consumed = if shared == 1 {if Buggy == 5 {0} else {1}} else {0};
                token = if shared == 1 {if Buggy == 6 {0} else {1}} else {0};
            }
            invariant CommittedParent: returned == 0 || exact == 1;
            invariant WriterExclusion: phase == 0 || excluded == 1;
            invariant WriterReleased: released == 1;
            invariant NewerParentRefused: returned == 0 || stale == 1;
            invariant ConsumptionRetained: phase == 0 || consumed == shared;
            invariant TokenRetained: phase == 0 || token == shared;
            invariant ResultPolicy: phase == 0 || (case == 4 && returned == 0)
                || (4 > case && returned == 1);
        }
    }
}

/// Local grant consumption observes authority under the account writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shared_begin_model() -> Model {
    aterm_spec::ty_model! {
        AccountSharedBegin {
            const Buggy = 0;
            var case = 0; var locked = 0; var consumed = 0; var reserved = 0; var token = 0;
            action Current when (case == 0) {
                case = 1; locked = if Buggy == 1 {0} else {1};
                consumed = if Buggy == 5 {0} else {1};
                reserved = if Buggy == 5 {0} else {1};
                token = if Buggy == 5 || Buggy == 6 {0} else {1};
            }
            action Stopped when (case == 0) {
                case = 2; locked = if Buggy == 1 {0} else {1};
                consumed = if Buggy == 2 {1} else {0};
                reserved = if Buggy == 2 {1} else {0};
                token = if Buggy == 2 {1} else {0};
            }
            action ChangedHead when (case == 0) {
                case = 3; locked = if Buggy == 1 {0} else {1};
                consumed = if Buggy == 3 {1} else {0};
                reserved = if Buggy == 3 {1} else {0};
                token = if Buggy == 3 {1} else {0};
            }
            action Unavailable when (case == 0) {
                case = 4; locked = if Buggy == 1 {0} else {1};
                consumed = if Buggy == 4 {1} else {0};
                reserved = if Buggy == 4 {1} else {0};
                token = if Buggy == 4 {1} else {0};
            }
            action Unrelated when (case == 0) {
                case = 5; locked = if Buggy == 1 {0} else {1};
                consumed = if Buggy == 5 {0} else {1};
                reserved = if Buggy == 5 {0} else {1};
                token = if Buggy == 5 || Buggy == 6 {0} else {1};
            }
            invariant AuthorityWhileLocked: case == 0 || locked == 1;
            invariant OnlyCurrentBegin: case <= 1 || case == 5 || (consumed == 0 && reserved == 0 && token == 0);
            invariant EligibleProgress: case == 0 || (case > 1 && case <= 4) || consumed == 1;
            invariant AtomicCustody: consumed == reserved && consumed == token;
        }
    }
}

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
