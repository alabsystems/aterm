// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Database protocol models, including explicit unsafe negative controls.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
use aterm_spec::derive::Model;

/// Historical repair approval still needs an exact live channel identity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn control_admission_model() -> Model {
    aterm_spec::ty_model! {
        DbControlAdmission {
            const Buggy = 0;
            var scenario = 0;
            var admitted = 0;
            action Approved when (scenario == 0) { scenario = 1; admitted = if Buggy == 1 { 0 } else { 1 }; }
            action MissingProof when (scenario == 0) { scenario = 2; admitted = if Buggy == 2 { 1 } else { 0 }; }
            action CopiedWitness when (scenario == 0) { scenario = 3; admitted = if Buggy == 3 { 1 } else { 0 }; }
            action ForeignPins when (scenario == 0) { scenario = 4; admitted = if Buggy == 4 { 1 } else { 0 }; }
            action ForeignGenesis when (scenario == 0) { scenario = 5; admitted = if Buggy == 5 { 1 } else { 0 }; }
            action Unapproved when (scenario == 0) { scenario = 6; admitted = if Buggy == 6 { 1 } else { 0 }; }
            action ReflectedWriter when (scenario == 0) { scenario = 7; admitted = if Buggy == 7 { 1 } else { 0 }; }
            invariant OnlyApprovedAdmits: admitted == 0 || scenario == 1;
            invariant ApprovedWorks: scenario == 0 || scenario > 1 || admitted == 1;
        }
    }
}

/// A stable missing inventory is swept even if its first page is refused.
/// Successful pages leave that inventory; deferred records remain retryable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn control_repair_model() -> Model {
    aterm_spec::ty_model! {
        DbControlRepair {
            const Buggy = 0;
            var position = 0;
            var rounds = 0;
            var middle = 0;
            var last = 0;
            action RefusedFirst when (position == 0 && rounds <= 2) {
                rounds = rounds + 1;
                position = if Buggy == 1 { 0 } else { 1 };
            }
            action DeliveredMiddle when (position == 1 && rounds <= 2) {
                rounds = rounds + 1;
                middle = if Buggy == 2 { 0 } else { 1 };
                position = 2;
            }
            action DeliveredLast when (position == 2 && rounds <= 2) {
                rounds = rounds + 1;
                last = if Buggy == 3 { 0 } else { 1 };
                position = 0;
            }
            invariant NoStarvation: rounds <= 2 || (middle == 1 && last == 1);
        }
    }
}

/// Roster key release requires valid current membership, basis facts and epoch.
/// These cases are exercised through signed journals and actual key capsules.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn roster_access_model() -> Model {
    aterm_spec::ty_model! {
        DbRosterAccess {
            const Buggy = 0;
            var scenario = 0;
            var exposed = 0;
            action Valid when (scenario == 0) { scenario = 1; exposed = if Buggy == 1 { 0 } else { 1 }; }
            action RetiredMember when (scenario == 0) { scenario = 2; exposed = if Buggy == 2 { 1 } else { 0 }; }
            action StaleHandle when (scenario == 0) { scenario = 3; exposed = if Buggy == 3 { 1 } else { 0 }; }
            action MissingBasisFact when (scenario == 0) { scenario = 4; exposed = if Buggy == 4 { 1 } else { 0 }; }
            action WrongCapsule when (scenario == 0) { scenario = 5; exposed = if Buggy == 5 { 1 } else { 0 }; }
            action ForeignRecipient when (scenario == 0) { scenario = 6; exposed = if Buggy == 6 { 1 } else { 0 }; }
            action ForkedHistory when (scenario == 0) { scenario = 7; exposed = if Buggy == 7 { 1 } else { 0 }; }
            action OldEpoch when (scenario == 0) { scenario = 8; exposed = if Buggy == 8 { 1 } else { 0 }; }
            action DuplicateMember when (scenario == 0) { scenario = 9; exposed = if Buggy == 9 { 1 } else { 0 }; }
            action CascadedRecovery when (scenario == 0) { scenario = 10; exposed = if Buggy == 10 { 1 } else { 0 }; }
            action RepeatedEpoch when (scenario == 0) { scenario = 11; exposed = if Buggy == 11 { 1 } else { 0 }; }
            invariant OnlyValidatedRosterReleases: exposed == 0 || scenario == 1;
            invariant ValidRosterWorks: scenario == 0 || scenario > 1 || exposed == 1;
        }
    }
}

/// Device evidence is retained across recovery; retirement and ambiguity
/// withhold authorization regardless of the chosen encrypted snapshot.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn binding_ledger_model() -> Model {
    aterm_spec::ty_model! {
        DbBindingLedger {
            const Buggy = 0;
            var bound = 0;
            var retired = 0;
            var conflict = 0;
            var recovered = 0;
            var allowed = 1;
            var recorded = 0;
            action ObserveBinding when (bound == 0) {
                bound = 1;
                allowed = if Buggy == 5 { 0 } else { if retired == 0 && conflict == 0 { 1 } else { 0 } };
                recorded = if Buggy == 6 { 0 } else { if Buggy == 4 { 1 } else { if retired == 0 && conflict == 0 { 1 } else { 0 } } };
            }
            action ObserveRetirement when (retired == 0) {
                retired = 1;
                allowed = if Buggy == 1 { allowed } else { 0 };
                recorded = 0;
            }
            action ObserveConflict when (bound == 1 && conflict == 0) {
                conflict = 1;
                allowed = if Buggy == 2 { 1 } else { 0 };
                recorded = 0;
            }
            action ObserveWriterConflict when (bound == 1 && conflict == 0) {
                conflict = 2;
                allowed = if Buggy == 2 { 1 } else { 0 };
                recorded = 0;
            }
            action Recover when (recovered == 0) {
                recovered = 1;
                allowed = if Buggy == 3 { 1 } else { if retired == 0 && conflict == 0 { 1 } else { 0 } };
                recorded = if bound == 1 && retired == 0 && conflict == 0 { 1 } else { 0 };
            }
            invariant RetiredRecipientCannotReturn: retired == 0 || allowed == 0;
            invariant AmbiguousBindingWithholds: conflict == 0 || allowed == 0;
            invariant RecordedNeedsEvidence: recorded == 0 || (bound == 1 && retired == 0 && conflict == 0);
            invariant UnrestrictedRemainsUsable: retired == 1 || conflict > 0 || allowed == 1;
            invariant ExactLiveBindingWorks: bound == 0 || retired == 1 || conflict > 0 || recorded == 1;
        }
    }
}

/// Certification requires the current pinned authority, recipient and live
/// writer identity. Possession itself is a separate verified protocol.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn member_certificate_model() -> Model {
    aterm_spec::ty_model! {
        DbMemberCertificate {
            const Buggy = 0;
            var scenario = 0;
            var approved = 0;
            action Valid when (scenario == 0) {
                scenario = 1; approved = if Buggy == 5 { 0 } else { 1 };
            }
            action ForeignAuthority when (scenario == 0) {
                scenario = 2; approved = if Buggy == 1 { 1 } else { 0 };
            }
            action ForeignRecipient when (scenario == 0) {
                scenario = 3; approved = if Buggy == 2 { 1 } else { 0 };
            }
            action RetiredWriter when (scenario == 0) {
                scenario = 4; approved = if Buggy == 3 { 1 } else { 0 };
            }
            action IncompleteControl when (scenario == 0) {
                scenario = 5; approved = if Buggy == 4 { 1 } else { 0 };
            }
            action IneligibleRecipient when (scenario == 0) {
                scenario = 6; approved = if Buggy == 6 { 1 } else { 0 };
            }
            invariant ApprovalRequiresChecks: approved == 0 || scenario == 1;
            invariant ValidApprovalWorks: scenario == 0 || scenario > 1 || approved == 1;
        }
    }
}

/// A cutoff withdraws an operation and every causal dependent, regardless of
/// their authors or the order in which the evidence arrives.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn catalog_cutoff_model() -> Model {
    aterm_spec::ty_model! {
        DbCatalogCutoff {
            const Buggy = 0;
            var parent = 0;
            var child = 0;
            var cutoff = 0;
            var visible_parent = 0;
            var visible_child = 0;
            action ReceiveParent when (parent == 0) {
                parent = 1;
                visible_parent = if cutoff == 0 { 1 } else { 0 };
                visible_child = if child == 1 && cutoff == 0 { 1 } else { 0 };
            }
            action ReceiveChild when (child == 0) {
                child = 1;
                visible_child = if Buggy == 1 || (parent == 1 && cutoff == 0) { 1 } else { 0 };
            }
            action ReceiveCutoff when (cutoff == 0) {
                cutoff = 1;
                visible_parent = if Buggy == 3 { visible_parent } else { 0 };
                visible_child = if Buggy == 2 { visible_child } else { 0 };
            }
            invariant CutoffWithholds: cutoff == 0 || visible_parent == 0;
            invariant CausalClosure: visible_child == 0 || visible_parent == 1;
            invariant DependentWithheld: cutoff == 0 || visible_child == 0;
        }
    }
}

/// A numerically high concurrent epoch is not a post-retirement rotation.
/// Repeating a known retirement must not undo a rotation that already observed it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retirement_epoch_model() -> Model {
    aterm_spec::ty_model! {
        DbRetirementEpoch {
            const Buggy = 0;
            var retired = 0;
            var concurrent = 0;
            var rotated = 0;
            var repeated = 0;
            var allowed = 1;
            action Retire when (retired == 0) { retired = 1; allowed = 0; }
            action ConcurrentEpoch when (concurrent == 0) {
                concurrent = 1;
                allowed = if Buggy == 1 { 1 } else { allowed };
            }
            action Rotate when (retired == 1 && rotated == 0) { rotated = 1; allowed = 1; }
            action RepeatRetirement when (retired == 1 && repeated == 0) {
                repeated = 1;
                allowed = if Buggy == 2 { 0 } else { allowed };
            }
            invariant RotationRequired: retired == 0 || rotated == 1 || allowed == 0;
            invariant RotationSurvivesDuplicate: rotated == 0 || allowed == 1;
        }
    }
}

/// Retirements survive unrelated updates and selection of another branch.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retirement_union_model() -> Model {
    aterm_spec::ty_model! {
        DbRetirementUnion {
            const Buggy = 0;
            var retired = 0;
            var other = 0;
            var recovered = 0;
            var allowed = 1;
            action ObserveRetirement when (retired == 0) { retired = 1; allowed = 0; }
            action ObserveOther when (other == 0) {
                other = 1;
                allowed = if Buggy == 2 { 1 } else { if retired == 1 { 0 } else { 1 } };
            }
            action Recover when (recovered == 0) {
                recovered = 1;
                allowed = if Buggy == 1 { 1 } else { if retired == 1 { 0 } else { 1 } };
            }
            invariant RetirementSurvives: retired == 0 || allowed == 0;
        }
    }
}

/// Key epochs merge independently of delivery order. Highest-generation forks
/// and reusing an older key withhold use until a fresh higher epoch arrives.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn key_epoch_union_model() -> Model {
    aterm_spec::ty_model! {
        DbKeyEpochUnion {
            const Buggy = 0;
            var old = 0;
            var newer = 0;
            var conflict = 0;
            var fresh = 0;
            var reused = 0;
            var usable = 0;
            action ObserveOld when (old == 0) {
                old = 1;
                usable = if reused == 1 {
                    if Buggy == 3 { 4 } else { 0 }
                } else { if Buggy == 1 { 1 } else {
                    if fresh == 1 { 3 } else {
                        if newer == 1 && conflict == 1 { if Buggy == 2 { 2 } else { 0 } }
                        else { if newer == 1 || conflict == 1 { 2 } else { 1 } }
                    }
                } };
            }
            action ObserveNew when (newer == 0) {
                newer = 1;
                usable = if reused == 1 {
                    if old == 1 { if Buggy == 3 { 4 } else { 0 } } else { 4 }
                } else { if fresh == 1 { 3 } else { if conflict == 1 { if Buggy == 2 { 2 } else { 0 } } else { 2 } } };
            }
            action ObserveConflict when (conflict == 0) {
                conflict = 1;
                usable = if reused == 1 {
                    if old == 1 && Buggy == 3 { 4 } else { if old == 1 { 0 } else { 4 } }
                } else { if fresh == 1 { 3 } else { if newer == 1 { if Buggy == 2 { 2 } else { 0 } } else { 2 } } };
            }
            action ObserveFresh when (fresh == 0) {
                fresh = 1;
                usable = if reused == 1 { if old == 1 && Buggy == 3 { 4 } else { if old == 1 { 0 } else { 4 } } } else { 3 };
            }
            action ObserveReuse when (reused == 0) {
                reused = 1;
                usable = if old == 1 { if Buggy == 3 { 4 } else { 0 } } else { 4 };
            }
            invariant OldKeyNotReused: reused == 0 || old == 0 || usable == 0;
            invariant FreshDominates: reused == 1 || fresh == 0 || usable == 3;
            invariant ConflictWithholds: reused == 1 || fresh == 1 || newer == 0 || conflict == 0 || usable == 0;
            invariant NewDominates: reused == 1 || fresh == 1 || newer == 0 || conflict == 1 || usable == 2;
        }
    }
}

/// One volatile possession challenge. Failure preserves a live challenge, success
/// consumes it, and expiry before success prevents any later proof. Four mutants
/// accept an invalid signature, reuse success, ignore expiry, or consume failure.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn device_possession_model() -> Model {
    aterm_spec::ty_model! {
        DbDevicePossession {
            const Buggy = 0;
            var issued = 0;
            var pending = 0;
            var proofs = 0;
            var invalid = 0;
            var expired = 0;
            action Issue when (issued == 0) { issued = 1; pending = 1; }
            action ValidProof when (issued == 1 && 2 > proofs) {
                proofs = if pending == 1 { proofs + 1 } else { proofs };
                pending = if pending == 1 && Buggy == 2 { 1 } else { 0 };
            }
            action InvalidProof when (issued == 1) {
                invalid = if Buggy == 1 && pending == 1 { 1 } else { invalid };
                pending = if Buggy == 4 { 0 } else { pending };
            }
            action Expire when (issued == 1) {
                expired = if proofs == 0 { 1 } else { expired };
                pending = if Buggy == 3 && pending == 1 { 1 } else { 0 };
            }
            invariant InvalidNeverProves: invalid == 0;
            invariant SingleUse: proofs <= 1;
            invariant ExpiredNeverProves: expired == 0 || proofs == 0;
            invariant FailurePreservesChallenge: issued == 0 || proofs > 0 || expired == 1 || pending == 1;
        }
    }
}

/// Two machines each own two units. A partition never increases either share.
/// Mutant one spends a peer's apparently idle share; mutant two spends twice.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn offline_rights_model() -> Model {
    aterm_spec::ty_model! {
        DbOfflineRights {
            const Buggy = 0;
            var a = 0;
            var b = 0;
            var disconnected = 0;
            action Separate when (disconnected == 0) { disconnected = 1; }
            action Reconnect when (disconnected == 1) { disconnected = 0; }
            action SpendA when (2 > a || (Buggy == 1 && disconnected == 1 && 4 > a)) {
                a = a + 1;
            }
            action SpendB when (2 > b || (Buggy == 2 && 4 > b)) { b = b + 1; }
            invariant ShareA: a <= 2;
            invariant ShareB: b <= 2;
            invariant Conserved: a + b <= 4;
        }
    }
}

/// A signed child's visibility requires its parent. Delivery order is arbitrary.
/// Learning fork evidence withdraws both the author and dependent visibility.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn causal_visibility_model() -> Model {
    aterm_spec::ty_model! {
        DbCausalVisibility {
            const Buggy = 0;
            var parent = 0;
            var child = 0;
            var fork = 0;
            var visible_parent = 0;
            var visible_child = 0;
            action ReceiveParent when (parent == 0) {
                parent = 1;
                visible_parent = if fork == 0 { 1 } else { 0 };
                visible_child = if child == 1 && fork == 0 { 1 } else { 0 };
            }
            action ReceiveChild when (child == 0) {
                child = 1;
                visible_child = if Buggy == 1 || (parent == 1 && fork == 0) { 1 } else { 0 };
            }
            action ReceiveFork when (fork == 0) {
                fork = 1;
                visible_parent = 0;
                visible_child = if Buggy == 2 { visible_child } else { 0 };
            }
            invariant CausalClosure: visible_child == 0 || visible_parent == 1;
            invariant ForkStopsDependents: fork == 0 || visible_child == 0;
        }
    }
}

/// Two competing control updates, one descendant, and a signed recovery.
/// Arrival order is arbitrary; incomplete ancestry and forks withhold readiness.
/// Mutants select a fork winner or trust a recovery before its parents arrive.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn membership_journal_model() -> Model {
    aterm_spec::ty_model! {
        DbMembershipJournal {
            const Buggy = 0;
            var left = 0;
            var right = 0;
            var tail = 0;
            var recovery = 0;
            var visible = 1;
            action ReceiveLeft when (left == 0) {
                left = 1;
                visible = if recovery == 1 {
                    if right == 1 && tail == 1 { 5 } else { 0 }
                } else {
                    if right == 1 || tail == 1 { 0 } else { 2 }
                };
            }
            action ReceiveRight when (right == 0) {
                right = 1;
                visible = if recovery == 1 {
                    if left == 1 && tail == 1 { 5 } else { 0 }
                } else {
                    if left == 1 {
                        if Buggy == 1 { 3 } else { 0 }
                    } else {
                        if tail == 1 { 4 } else { 3 }
                    }
                };
            }
            action ReceiveTail when (tail == 0) {
                tail = 1;
                visible = if recovery == 1 {
                    if left == 1 && right == 1 { 5 } else { 0 }
                } else {
                    if right == 0 {
                        if Buggy == 2 { 4 } else { 0 }
                    } else {
                        if left == 1 {
                            if Buggy == 1 { 4 } else { 0 }
                        } else { 4 }
                    }
                };
            }
            action ReceiveRecovery when (recovery == 0) {
                recovery = 1;
                visible = if Buggy == 2 || (left == 1 && right == 1 && tail == 1) { 5 } else { 0 };
            }
            invariant DescendantHasParent: 4 > visible || visible > 4 || right == 1;
            invariant RecoveryHasAncestry: 5 > visible || visible > 5 || (left == 1 && right == 1 && tail == 1);
            invariant ForkHasNoWinner: left == 0 || right == 0 || recovery == 1 || visible == 0;
        }
    }
}
