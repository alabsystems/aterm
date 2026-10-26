// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Database protocol models, including explicit unsafe negative controls.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
use aterm_spec::derive::Model;

mod account_plan;
pub use account_plan::account_plan_model;
mod account_activation;
pub use account_activation::account_activation_model;
mod verified_entry;
pub use verified_entry::verified_entry_model;

mod dispatch;
pub use dispatch::resource_dispatch_model;
mod discovery;
pub use discovery::discovery_replacement_model;
mod enrollment_gate;
pub use enrollment_gate::enrollment_gate_model;

/// Bounded executions of the durable fixed-share reservation API. Initial pins
/// are assumed coherent across consumers; this model grants no provider access.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn resource_reservation_model() -> Model {
    aterm_spec::ty_model! {
        DbResourceReservation {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var fresh = 0;
            var durable = 0; var same = 1; var used = 0; var capacity = 3;
            action Reserve when (scenario == 0) {
                scenario = 1; accepted = if Buggy == 6 {0} else {1}; fresh = 1;
                durable = if Buggy == 1 {0} else {1}; used = if Buggy == 5 {4} else {3};
            }
            action Retry when (scenario == 0) {
                scenario = 2; accepted = 1; fresh = if Buggy == 2 {1} else {0};
                durable = 1; same = if Buggy == 3 {0} else {1}; used = 2;
            }
            action WrongCost when (scenario == 0) {
                scenario = 3; accepted = if Buggy == 4 {1} else {0}; durable = 1; used = 2;
            }
            action Exhausted when (scenario == 0) {
                scenario = 4; accepted = if Buggy == 7 {1} else {0}; durable = 1; used = 3;
            }
            action WrongPin when (scenario == 0) {
                scenario = 5; accepted = if Buggy == 8 {1} else {0}; durable = 1; used = 2;
            }
            action BadHistory when (scenario == 0) {
                scenario = 6; accepted = if Buggy == 9 {1} else {0}; durable = 1; used = 2;
            }
            action NoShare when (scenario == 0) {
                scenario = 7; accepted = if Buggy == 10 {1} else {0};
            }
            action TwoOffline when (scenario == 0) {
                scenario = 8; accepted = 1; fresh = 1; durable = 1; used = 5; capacity = 5;
            }
            action Uninitialized when (scenario == 0) {
                scenario = 9; accepted = if Buggy == 11 {1} else {0};
            }
            action NoPin when (scenario == 0) {
                scenario = 10; accepted = if Buggy == 12 {1} else {0};
            }
            invariant DurableReceipt: accepted == 0 || durable == 1;
            invariant RetryIsNotFresh: 2 > scenario || scenario > 2 || fresh == 0;
            invariant RetryKeepsReceipt: 2 > scenario || scenario > 2 || same == 1;
            invariant RefusalNeverGrants: 3 > scenario || scenario == 8 || accepted == 0;
            invariant FixedCapacity: used <= capacity;
            invariant EligibleProgress: ((1 > scenario || scenario > 1) && (8 > scenario || scenario > 8)) || (accepted == 1 && fresh == 1);
        }
    }
}

/// A reference diagnostic identifies observed ambiguity without granting rights
/// or changing proposals. Traversal limits remain visible in the report.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_references_model() -> Model {
    aterm_spec::ty_model! {
        DbAccountReferences {
            const Buggy = 0;
            var scenario = 0; var warning = 0; var truncated = 0; var effects = 0;
            action Clean when (scenario == 0) {
                scenario = 1; warning = if Buggy == 8 {1} else {0}; effects = if Buggy == 9 {1} else {0};
            }
            action Cycle when (scenario == 0) {scenario = 2; warning = if Buggy == 1 {0} else {1};}
            action Conflict when (scenario == 0) {scenario = 3; warning = if Buggy == 2 {0} else {1};}
            action Deleted when (scenario == 0) {scenario = 4; warning = if Buggy == 3 {0} else {1};}
            action Route when (scenario == 0) {scenario = 5; warning = if Buggy == 4 {0} else {1};}
            action Duplicate when (scenario == 0) {scenario = 6; warning = if Buggy == 5 {0} else {1};}
            action Namespace when (scenario == 0) {scenario = 7; warning = if Buggy == 6 {0} else {1};}
            action Limit when (scenario == 0) {scenario = 8; warning = 1; truncated = if Buggy == 7 {0} else {1};}
            action DetailMissing when (scenario == 0) {scenario = 9; warning = if Buggy == 10 {0} else {1};}
            action DetailAlias when (scenario == 0) {scenario = 10; warning = if Buggy == 11 {0} else {1};}
            action DetailClean when (scenario == 0) {scenario = 11; warning = if Buggy == 12 {1} else {0};}
            action DetailSyntax when (scenario == 0) {scenario = 12; warning = if Buggy == 13 {0} else {1};}
            invariant ReportReferences: 2 > scenario || scenario > 7 || warning == 1;
            invariant ReportDetailReferences: 9 > scenario || scenario == 11 || scenario > 12 || warning == 1;
            invariant NoInventedRequirement: ((1 > scenario || scenario > 1) && (11 > scenario || scenario > 11)) || warning == 0;
            invariant BoundedInspection: 8 > scenario || scenario > 8 || truncated == 1;
            invariant NoEffects: effects == 0;
        }
    }
}

/// Recipient retirement survives a delayed binding and recovery of a concurrent
/// enrollment branch; only a fresh covering epoch permits continued local use.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_retirement_model() -> Model {
    aterm_spec::ty_model! {
        DbRecipientRetirement {
            const Buggy = 0;
            var scenario = 0; var retired = 0; var allowed = 0; var old_ready = 0; var ready = 1;
            action LateBinding when (scenario == 0) {
                scenario = 1; retired = 1; allowed = if Buggy == 1 {1} else {0};
            }
            action ConcurrentRecovery when (scenario == 0) {
                scenario = 2; retired = if Buggy == 2 {0} else {1};
                old_ready = if Buggy == 3 {1} else {0}; ready = if Buggy == 4 {0} else {1};
            }
            action UnboundEpoch when (scenario == 0) {
                scenario = 3; retired = 1; old_ready = if Buggy == 5 {1} else {0};
                ready = if Buggy == 6 {0} else {1};
            }
            invariant RetirementPersists: scenario == 0 || retired == 1;
            invariant NoFutureBinding: allowed == 0;
            invariant OldKeyWithheld: old_ready == 0;
            invariant FreshKeyProgress: ready == 1;
        }
    }
}

/// Vault removal follows retained Commons retirement; retries never mint an
/// extra epoch and a failed publication cannot restore a retired device.
/// `controlled` is Commons control: a control entry the published document or
/// the edit names, or Commons state the admin's Mac keeps. The custody marker
/// alone is not control, so a vault without control removes vault-only.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retirement_publication_model() -> Model {
    aterm_spec::ty_model! {
        DbRetirementPublication {
            const Buggy = 0;
            var scenario = 0; var controlled = 0; var retired = 0; var published = 0; var same = 1;
            action Revoke when (scenario == 0) {
                scenario = 1; controlled = 1; retired = if Buggy == 1 {0} else {1}; published = if Buggy == 10 {0} else {1}; same = 0;
            }
            action PublishFailure when (scenario == 0) {
                scenario = 2; controlled = 1; retired = if Buggy == 2 {0} else {1}; published = 0; same = 0;
            }
            action Retry when (scenario == 0) {
                scenario = 3; controlled = 1; retired = 1; published = 1; same = if Buggy == 3 {0} else {1};
            }
            action NoBinding when (scenario == 0) {
                scenario = 4; controlled = 1; retired = if Buggy == 4 {0} else {1}; published = 1; same = 0;
            }
            action Legacy when (scenario == 0) {
                scenario = 5; controlled = 0; retired = if Buggy == 5 {1} else {0}; published = 1; same = 1;
            }
            action SelfRemoval when (scenario == 0) {
                scenario = 6; controlled = 1; retired = if Buggy == 6 {1} else {0}; published = 0; same = 1;
            }
            action Admin when (scenario == 0) {
                scenario = 7; controlled = 1; retired = if Buggy == 7 {1} else {0}; published = 0; same = 1;
            }
            action Fork when (scenario == 0) {
                scenario = 8; controlled = 1; retired = if Buggy == 8 {1} else {0}; published = 0; same = 1;
            }
            action MissingIdentity when (scenario == 0) {
                scenario = 9; controlled = 1; retired = if Buggy == 9 {1} else {0}; published = 0; same = 1;
            }
            action RepairRemoved when (scenario == 0) {
                scenario = 10; controlled = 1; retired = if Buggy == 11 {0} else {1}; published = 1; same = 0;
            }
            action BatchRemoved when (scenario == 0) {
                scenario = 11; controlled = 1; retired = if Buggy == 12 {0} else {1}; published = 1; same = 0;
            }
            action UnknownBatch when (scenario == 0) {
                scenario = 12; controlled = 1; retired = if Buggy == 13 {1} else {0}; published = 0; same = 1;
            }
            action GenericRemoval when (scenario == 0) {
                scenario = 13; controlled = 1; retired = 0; published = if Buggy == 14 {1} else {0}; same = 1;
            }
            action Unfounded when (scenario == 0) {
                scenario = 14; controlled = 0; retired = 0; published = if Buggy == 15 {0} else {1}; same = 1;
            }
            action RetainedRoot when (scenario == 0) {
                scenario = 15; controlled = 1; retired = 0; published = if Buggy == 16 {1} else {0}; same = 1;
            }
            action UnfoundedEdit when (scenario == 0) {
                scenario = 16; controlled = 0; retired = 0; published = if Buggy == 17 {0} else {1}; same = 1;
            }
            invariant RetireBeforeRemoval: controlled == 0 || published == 0 || retired == 1;
            invariant FailedPublicationRetains: 2 > scenario || scenario > 2 || retired == 1;
            invariant RetryKeepsEpoch: 3 > scenario || scenario > 3 || same == 1;
            invariant NoInventedRetirement: controlled == 1 || retired == 0;
            invariant PreflightProtects: 6 > scenario || scenario > 9 || (retired == 0 && published == 0 && same == 1);
            invariant UnknownBatchInert: 12 > scenario || scenario > 13 || (retired == 0 && published == 0 && same == 1);
            invariant RetainedControlRefuses: 15 > scenario || scenario > 15 || (retired == 0 && published == 0 && same == 1);
            invariant Progress: scenario == 0 || scenario == 2 || (scenario > 5 && 14 > scenario) || scenario == 15 || published == 1;
        }
    }
}

/// Enrollment publishes only after durable control ingestion; exact retries
/// reuse the existing binding while current exclusions and faults refuse.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn enrollment_commit_model() -> Model {
    aterm_spec::ty_model! {
        DbEnrollmentCommit {
            const Buggy = 0;
            var scenario = 0; var granted = 0; var retained = 0; var same = 1;
            action Enroll when (scenario == 0) {
                scenario = 1; granted = if Buggy == 8 { 0 } else { 1 }; retained = if Buggy == 1 { 0 } else { 1 }; same = 0;
            }
            action LostReply when (scenario == 0) {
                scenario = 2; granted = 1; retained = 1; same = if Buggy == 2 { 0 } else { 1 };
            }
            action Retry when (scenario == 0) {
                scenario = 3; granted = 1; retained = 1; same = if Buggy == 3 { 0 } else { 1 };
            }
            action NoEvidence when (scenario == 0) {
                scenario = 4; granted = if Buggy == 4 { 1 } else { 0 }; same = 1;
            }
            action Excluded when (scenario == 0) {
                scenario = 5; granted = if Buggy == 5 { 1 } else { 0 }; same = 1;
            }
            action Retired when (scenario == 0) {
                scenario = 6; granted = if Buggy == 6 { 1 } else { 0 }; same = 1;
            }
            action Fork when (scenario == 0) {
                scenario = 7; granted = if Buggy == 7 { 1 } else { 0 }; same = 1;
            }
            invariant RetainBeforeGrant: granted == 0 || retained == 1;
            invariant EligibleProgress: scenario == 0 || scenario > 3 || granted == 1;
            invariant RefusalCannotGrant: 4 > scenario || granted == 0;
            invariant RetryKeepsEpoch: 2 > scenario || same == 1;
        }
    }
}

/// Durable admission evidence is independent of editable machine lists and
/// later join policy. It authorizes only through current membership checks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn admission_evidence_model() -> Model {
    aterm_spec::ty_model! {
        DbAdmissionEvidence {
            const Buggy = 0;
            var scenario = 0; var evidence = 0; var allowed = 0;
            action Password when (scenario == 0) {
                scenario = 1; evidence = 2; allowed = if Buggy == 1 { 0 } else { 1 };
            }
            action Manual when (scenario == 0) {
                scenario = 2; evidence = 2; allowed = if Buggy == 2 { 0 } else { 1 };
            }
            action OpenClosed when (scenario == 0) {
                scenario = 3; evidence = 1; allowed = if Buggy == 3 { 1 } else { 0 };
            }
            action Removed when (scenario == 0) {
                scenario = 4; evidence = if Buggy == 4 { 2 } else { 0 }; allowed = 0;
            }
            action Readded when (scenario == 0) {
                scenario = 5; evidence = 0; allowed = if Buggy == 5 { 1 } else { 0 };
            }
            action GenericEdit when (scenario == 0) {
                scenario = 6; evidence = 0; allowed = if Buggy == 6 { 1 } else { 0 };
            }
            action Rotate when (scenario == 0) {
                scenario = 7; evidence = 2; allowed = if Buggy == 7 { 0 } else { 1 };
            }
            action Rename when (scenario == 0) {
                scenario = 8; evidence = 2; allowed = if Buggy == 8 { 0 } else { 1 };
            }
            action Excluded when (scenario == 0) {
                scenario = 9; evidence = 2; allowed = if Buggy == 9 { 1 } else { 0 };
            }
            action Legacy when (scenario == 0) {
                scenario = 10; evidence = 0; allowed = if Buggy == 10 { 1 } else { 0 };
            }
            action Draft when (scenario == 0) {
                scenario = 11; evidence = 0; allowed = if Buggy == 11 { 1 } else { 0 };
            }
            action InvalidProof when (scenario == 0) {
                scenario = 12; evidence = 0; allowed = if Buggy == 12 { 1 } else { 0 };
            }
            action ForeignRoot when (scenario == 0) {
                scenario = 13; evidence = 2; allowed = if Buggy == 13 { 1 } else { 0 };
            }
            action Founder when (scenario == 0) {
                scenario = 14; evidence = 2; allowed = if Buggy == 14 { 0 } else { 1 };
            }
            invariant EvidenceRequired: allowed == 0 || evidence == 2;
            invariant QualifiedWorks: scenario == 0 || (scenario > 2 && (7 > scenario || scenario > 8) && 14 > scenario) || allowed == 1;
            invariant RemovalForgets: scenario == 0 || 4 > scenario || scenario > 5 || evidence == 0;
            invariant CurrentMembershipRequired: scenario == 0 || 9 > scenario || scenario == 10 || scenario == 11 || scenario == 12 || scenario == 14 || allowed == 0;
        }
    }
}

/// Retained creation intents survive retries without minting new authority or
/// replacing a newer local vault. Missing acknowledged history fails closed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn creation_outbox_model() -> Model {
    aterm_spec::ty_model! {
        DbCreationOutbox {
            const Buggy = 0;
            var scenario = 0; var retained = 0; var published = 0;
            var exact = 1; var refused = 0; var monotone = 1;
            var device_reset = 0;
            action Resume when (scenario == 0) {
                scenario = 1; retained = if Buggy == 1 { 0 } else { 1 }; published = 1;
                exact = if Buggy == 2 { 0 } else { 1 };
            }
            action ChangedImport when (scenario == 0) {
                scenario = 2; retained = 1; published = 1; exact = if Buggy == 3 { 0 } else { 1 };
            }
            action LostJournal when (scenario == 0) {
                scenario = 3; retained = 1; refused = if Buggy == 4 { 0 } else { 1 };
            }
            action ChangedIntent when (scenario == 0) {
                scenario = 4; retained = 1; refused = if Buggy == 5 { 0 } else { 1 };
            }
            action CompletedRetry when (scenario == 0) {
                scenario = 5; retained = 1; published = 1; monotone = if Buggy == 6 { 0 } else { 1 };
            }
            action NewerLocal when (scenario == 0) {
                scenario = 6; retained = 1; monotone = if Buggy == 7 { 0 } else { 1 };
            }
            action ChangedConfig when (scenario == 0) {
                scenario = 7; retained = 1; refused = if Buggy == 8 { 0 } else { 1 };
            }
            action BeforeReady when (scenario == 0) {
                scenario = 8; retained = 1; published = 1; exact = if Buggy == 9 { 0 } else { 1 };
            }
            action LostDevice when (scenario == 0) {
                scenario = 9; retained = 1; refused = if Buggy == 10 { 0 } else { 1 };
                device_reset = if Buggy == 11 { 1 } else { 0 };
            }
            invariant RetainBeforePublish: published == 0 || retained == 1;
            invariant ExactPublication: exact == 1;
            invariant NeverRollBack: monotone == 1;
            invariant DamagedOrForeignRefuses: 3 > scenario || (scenario > 4 && 7 > scenario) || scenario > 7 || refused == 1;
            invariant LostDeviceRefuses: 9 > scenario || scenario > 9 || refused == 1;
            invariant NoDeviceReset: device_reset == 0;
        }
    }
}

/// New vaults bind one Commons root and an explicitly authorized founder in
/// their first signed envelope. Later edits cannot replace that root.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foundation_birth_model() -> Model {
    aterm_spec::ty_model! {
        DbFoundationBirth {
            const Buggy = 0;
            var scenario = 0; var bound = 0; var stable = 1; var available = 0; var accepted = 0; var writer = 0;
            action Create when (scenario == 0) {
                scenario = 1; bound = if Buggy == 1 { 0 } else { 1 }; accepted = if Buggy == 10 { 0 } else { 1 };
                available = if Buggy == 7 { 0 } else { 1 };
                writer = if Buggy == 9 { 0 } else { 1 };
            }
            action Rotate when (scenario == 0) {
                scenario = 2; bound = 1; accepted = 1; available = 1; writer = 1; stable = if Buggy == 2 { 0 } else { 1 };
            }
            action RemoveRoot when (scenario == 0) {
                scenario = 3; bound = 1; available = 1; writer = 1; accepted = if Buggy == 3 { 1 } else { 0 };
            }
            action ReplaceRoot when (scenario == 0) {
                scenario = 4; bound = 1; available = 1; writer = 1; accepted = if Buggy == 4 { 1 } else { 0 };
            }
            action Enroll when (scenario == 0) {
                scenario = 5; bound = 1; accepted = 1; writer = 1; available = if Buggy == 5 { 0 } else { 1 };
            }
            action Legacy when (scenario == 0) {
                scenario = 6; bound = if Buggy == 6 { 1 } else { 0 }; available = if Buggy == 8 { 1 } else { 0 }; accepted = 1;
            }
            invariant BoundAtBirth: scenario == 0 || scenario == 6 || bound == 1;
            invariant StableRoot: stable == 1;
            invariant FounderAccess: scenario == 0 || scenario == 6 || available == 1;
            invariant FounderWriter: scenario == 0 || scenario == 6 || writer == 1;
            invariant LegacyNoGrant: scenario <= 5 || available == 0;
            invariant NoRootReplacement: scenario <= 2 || scenario > 4 || accepted == 0;
            invariant EnrolledAccess: scenario <= 4 || scenario > 5 || available == 1;
            invariant LegacyUnchanged: scenario <= 5 || scenario > 6 || bound == 0;
            invariant Progress: scenario == 0 || scenario == 3 || scenario == 4 || accepted == 1;
        }
    }
}

/// Public default foundations have one encoding and confer no data authority.
/// These finite codec scenarios assume the caller's independently trusted key.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foundation_model() -> Model {
    aterm_spec::ty_model! {
        DbFoundation {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var canonical = 1; var grants = 0;
            action Derive when (scenario == 0) { scenario = 1; accepted = 1; canonical = if Buggy == 1 { 0 } else { 1 }; }
            action RoundTrip when (scenario == 0) { scenario = 2; accepted = if Buggy == 2 { 0 } else { 1 }; }
            action Altered when (scenario == 0) { scenario = 3; accepted = if Buggy == 3 { 1 } else { 0 }; }
            action ForeignRoot when (scenario == 0) { scenario = 4; accepted = if Buggy == 4 { 1 } else { 0 }; }
            action ForeignNetwork when (scenario == 0) { scenario = 5; accepted = if Buggy == 5 { 1 } else { 0 }; }
            action NoAccess when (scenario == 0) { scenario = 6; grants = if Buggy == 6 { 1 } else { 0 }; }
            action NoHistorical when (scenario == 0) { scenario = 7; grants = if Buggy == 7 { 1 } else { 0 }; }
            action NonCanonicalDefault when (scenario == 0) { scenario = 8; accepted = if Buggy == 8 { 1 } else { 0 }; }
            invariant UniqueEncoding: canonical == 1;
            invariant ValidFoundationWorks: scenario == 0 || scenario > 2 || accepted == 1;
            invariant ForeignBytesRefused: scenario <= 2 || scenario == 6 || scenario == 7 || accepted == 0;
            invariant NoPublicGrant: grants == 0;
        }
    }
}

/// Two first rosters share one root, withhold keys when exchanged, and require
/// both explicit conflict recovery and a fresh key epoch to restore access.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foundation_repair_model() -> Model {
    aterm_spec::ty_model! {
        DbFoundationRepair {
            const Buggy = 0;
            var a = 0; var b = 0; var merged = 0; var recovered = 0; var fresh = 0;
            var available_a = 0; var available_b = 0; var root_same = 1;
            action FirstA when (a == 0 && merged == 0) {
                a = 1; available_a = if Buggy == 1 { 0 } else { 1 };
                root_same = if Buggy == 6 { 0 } else { 1 };
            }
            action FirstB when (b == 0 && merged == 0) {
                b = 1; available_b = if Buggy == 2 { 0 } else { 1 };
            }
            action Exchange when (a == 1 && b == 1 && merged == 0) {
                merged = 1; available_a = if Buggy == 3 { 1 } else { 0 }; available_b = 0;
            }
            action Recover when (merged == 1 && recovered == 0) {
                recovered = 1; available_b = if Buggy == 4 { 1 } else { 0 };
            }
            action Rotate when (recovered == 1 && fresh == 0) {
                fresh = 1; available_a = if Buggy == 5 { 0 } else { 1 }; available_b = 1;
            }
            invariant CommonRoot: root_same == 1;
            invariant FirstMemberA: merged == 1 || available_a == a;
            invariant FirstMemberB: merged == 1 || available_b == b;
            invariant ConflictWithholds: merged == 0 || fresh == 1 || available_a + available_b == 0;
            invariant FreshEpochRestores: fresh == 0 || available_a + available_b == 2;
        }
    }
}

/// Admin custody retains the Commons key across vault signing-key rotations.
/// These finite codec and signing scenarios do not prove crypto or key erasure.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn authority_custody_model() -> Model {
    aterm_spec::ty_model! {
        DbAuthorityCustody {
            const Buggy = 0;
            var scenario = 0; var usable = 0; var stable = 1; var separate = 0;
            action NewVault when (scenario == 0) {
                scenario = 1; usable = 1; separate = if Buggy == 1 { 0 } else { 1 };
            }
            action LegacyMigration when (scenario == 0) {
                scenario = 2; usable = 1; stable = if Buggy == 2 { 0 } else { 1 };
            }
            action VaultRotation when (scenario == 0) {
                scenario = 3; usable = if Buggy == 3 { 0 } else { 1 };
                separate = 1; stable = 1;
            }
            action LegacyRotation when (scenario == 0) {
                scenario = 4; usable = 1; separate = 1; stable = if Buggy == 4 { 0 } else { 1 };
            }
            action LostLegacyKey when (scenario == 0) {
                scenario = 5; usable = if Buggy == 5 { 1 } else { 0 };
            }
            action MissingCustody when (scenario == 0) {
                scenario = 6; usable = if Buggy == 6 { 1 } else { 0 };
            }
            action ChangedDescriptor when (scenario == 0) {
                scenario = 7; usable = if Buggy == 7 { 1 } else { 0 };
            }
            action UnboundRotation when (scenario == 0) {
                scenario = 8; usable = 1; separate = 1; stable = if Buggy == 8 { 1 } else { 0 };
            }
            action BoundDemotion when (scenario == 0) {
                scenario = 9; usable = if Buggy == 9 { 1 } else { 0 };
            }
            action RemovedDescriptor when (scenario == 0) {
                scenario = 10; usable = if Buggy == 10 { 1 } else { 0 };
            }
            action LegacyUnknown when (scenario == 0) {
                scenario = 11; usable = if Buggy == 11 { 1 } else { 0 };
            }
            invariant RetainedAuthority: scenario == 8 || stable == 1;
            invariant UnusedAuthorityRotates: 8 > scenario || scenario > 8 || stable == 0;
            invariant UsableCustodyWorks: scenario == 0 || (scenario > 4 && 8 > scenario) || scenario > 8 || usable == 1;
            invariant LostCustodyRefuses: scenario <= 4 || scenario == 8 || usable == 0;
            invariant IndependentKeys: scenario == 0 || scenario == 2 || (scenario > 4 && 8 > scenario) || scenario > 8 || separate == 1;
        }
    }
}

/// A Commons source document is published atomically and retried exactly.
/// Both import backends reject ambiguous source headers before writing records.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn import_atomic_model() -> Model {
    aterm_spec::ty_model! {
        DbImportAtomic {
            const Buggy = 0;
            var phase = 0; var published = 0; var effects = 0; var stable = 1;
            action Reject when (phase == 0) {
                phase = 1; published = if Buggy == 1 { 1 } else { 0 };
                effects = if Buggy == 5 { 1 } else { 0 };
            }
            action Commit when (phase == 0) {
                phase = 2; published = 1; effects = if Buggy == 2 { 0 } else { 1 };
            }
            action Retry when (phase == 2) {
                phase = 3; effects = if Buggy == 3 { 2 } else { 1 };
                stable = if Buggy == 4 { 0 } else { 1 };
            }
            invariant RejectBeforePublication: phase == 0 || phase > 1 || published == 0;
            invariant RejectBeforeWrites: phase == 0 || phase > 1 || effects == 0;
            invariant AtomicSource: phase <= 1 || effects > 0;
            invariant OneEffect: effects <= 1;
            invariant OriginalReceipt: stable == 1;
        }
    }
}

/// A source reselected by control is no longer quarantined, but its negative
/// receipt cannot admit positive data or let the author skip known history.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn source_reselection_model() -> Model {
    aterm_spec::ty_model! {
        DbSourceReselection {
            const Buggy = 0;
            var phase = 0; var stored = 1; var active = 1;
            var positive = 0; var writable = 0; var liability = 0;
            action Reselect when (phase == 0) {
                phase = 1;
                active = if Buggy == 1 { 1 } else { 0 };
                positive = if Buggy == 2 { 1 } else { 0 };
                stored = if Buggy == 3 { 0 } else { 1 };
                writable = if Buggy == 4 { 1 } else { 0 };
                liability = if Buggy == 6 { 1 } else { 0 };
            }
            action Admit when (phase == 1) {
                phase = 2; positive = 1;
                writable = if Buggy == 5 { 0 } else { 1 };
                liability = if Buggy == 7 { 0 } else { 1 };
            }
            invariant CurrentSelection: phase == 0 || active == 0;
            invariant RetainedEvidence: stored == 1;
            invariant NoImplicitAdmission: phase > 1 || positive == 0;
            invariant NoSequenceReuse: phase > 1 || writable == 0;
            invariant RecoveredAuthorProgress: phase <= 1 || writable == 1;
            invariant NoPrematureLiabilityRelease: phase > 1 || liability == 0;
            invariant RecoveredLiabilityProgress: phase <= 1 || liability == 1;
        }
    }
}

/// A retained request collision remains restrictive after positive membership
/// filtering or control-recovery quarantine. The latter fences only its author.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn proposal_evidence_model() -> Model {
    aterm_spec::ty_model! {
        DbProposalEvidence {
            const Buggy = 0;
            var scenario = 0; var retained = 0; var revived = 0;
            var retry = 0; var peer = 0; var converged = 0;
            action Withdraw when (scenario == 0) {
                scenario = 1; retained = if Buggy == 3 { 1 } else { 2 };
                revived = if Buggy == 1 { 1 } else { 0 };
                retry = if Buggy == 2 { 1 } else { 0 };
                converged = if Buggy == 5 { 0 } else { 1 };
            }
            action Quarantine when (scenario == 0) {
                scenario = 2; retained = if Buggy == 3 { 1 } else { 2 };
                revived = if Buggy == 1 { 1 } else { 0 };
                retry = if Buggy == 2 { 1 } else { 0 };
                peer = if Buggy == 4 { 0 } else { 1 };
                converged = if Buggy == 5 { 0 } else { 1 };
            }
            invariant EvidenceRetained: scenario == 0 || retained == 2;
            invariant NoRevival: revived == 0;
            invariant NoFalseRetry: retry == 0;
            invariant AuthorIsolation: scenario <= 1 || peer == 1;
            invariant NegativeRepair: scenario == 0 || converged == 1;
        }
    }
}

/// Retrying a caller-held request preserves one durable effect; changing its
/// contents is a refusal, never permission to reinterpret the receipt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn proposal_retry_model() -> Model {
    aterm_spec::ty_model! {
        DbProposalRetry {
            const Buggy = 0;
            var written = 0; var count = 0; var retried = 0; var changed = 0;
            action Write when (written == 0) {
                written = 1; count = if Buggy == 3 { 0 } else { 1 };
            }
            action Retry when (written == 1 && retried == 0) {
                retried = 1; count = if Buggy == 1 { 2 } else { count };
            }
            action Change when (written == 1 && changed == 0) {
                changed = 1; count = if Buggy == 2 { 2 } else { count };
            }
            invariant OneEffect: count <= 1;
            invariant DurableWrite: written == 0 || count == 1;
        }
    }
}

/// Default subscription gates under every ordering of discovery, conflict and
/// local policy changes. TLS delivery and storage have separate bounded models.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn catalog_subscription_model() -> Model {
    aterm_spec::ty_model! {
        DbCatalogSubscription {
            const Buggy = 0;
            var known = 0; var conflict = 0; var excluded = 0;
            var enabled = 1; var subscribed = 0;
            var overflow = 0;
            action Discover when (known == 0) {
                known = 1;
                subscribed = if Buggy == 7 { 0 } else { if Buggy == 1 || (excluded == 0 && enabled == 1) { 1 } else { 0 } };
            }
            action Conflict when (known == 1 && conflict == 0) {
                conflict = 1; subscribed = if Buggy == 2 { 1 } else { 0 };
            }
            action Exclude when (excluded == 0) {
                excluded = 1; subscribed = if Buggy == 3 { 1 } else { 0 };
            }
            action Include when (excluded == 1) {
                excluded = 0;
                subscribed = if known == 1 && conflict == 0 && (enabled == 1 || Buggy == 5) { 1 } else { 0 };
            }
            action Disable when (enabled == 1) {
                enabled = 0; subscribed = if Buggy == 4 { 1 } else { 0 };
            }
            action Enable when (enabled == 0) {
                enabled = 1;
                subscribed = if known == 1 && excluded == 0 && (conflict == 0 || Buggy == 6) { 1 } else { 0 };
            }
            action Quota when (known == 1 && overflow == 0) {
                overflow = 1; subscribed = if Buggy == 8 { 0 } else { subscribed };
            }
            invariant KnownBeforeSubscription: subscribed == 0 || known == 1;
            invariant ConflictBlocks: conflict == 0 || subscribed == 0;
            invariant ExclusionBlocks: excluded == 0 || subscribed == 0;
            invariant DisabledBlocks: enabled == 1 || subscribed == 0;
            invariant DefaultAvailable: known == 0 || conflict == 1 || excluded == 1 || enabled == 0 || subscribed == 1;
        }
    }
}

/// Two authenticated peers repeatedly acquire and release per-peer
/// leases. Progress persists without eviction; duplicate leases are refused.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_repair_cache_model() -> Model {
    aterm_spec::ty_model! {
        DbPeerRepairCache {
            const Buggy = 0;
            var a = 0;
            var b = 0;
            var wrote = 0;
            var duplicate = 0;
            action Acquire when (a == 0 || a == 2) {
                a = if Buggy == 3 && a == 2 { 1 } else { a + 1 };
            }
            action Duplicate when (a == 1 || a == 3) {
                duplicate = if Buggy == 1 { 1 } else { 0 };
            }
            action Advance when (a == 1) { a = 3; wrote = 1; }
            action Release when (a == 1 || a == 3) {
                a = if Buggy == 2 && a == 3 { 0 } else { a - 1 };
            }
            action OtherAcquire when (b == 0) {
                b = 1; a = if Buggy == 4 { 0 } else { a };
            }
            action OtherRelease when (b == 1) { b = 0; }
            invariant ExclusiveLease: duplicate == 0;
            invariant RetainedProgress: wrote == 0 || a > 1;
        }
    }
}

/// Actual TLS repair and durable-receipt scenarios. The model bounds these
/// scenarios; it does not prove cryptography, unbounded liveness or disk hardware.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_repair_model() -> Model {
    aterm_spec::ty_model! {
        DbTableRepair {
            const Buggy = 0;
            var scenario = 0;
            var stored = 0;
            var acknowledged = 0;
            var control = 0;
            action NewMember when (scenario == 0) { scenario = 1; stored = if Buggy == 1 { 0 } else { 1 }; acknowledged = 1; control = 1; }
            action LostKey when (scenario == 0) { scenario = 2; stored = if Buggy == 2 { 0 } else { 1 }; acknowledged = 1; control = 1; }
            action Legacy when (scenario == 0) { scenario = 3; stored = if Buggy == 3 { 1 } else { 0 }; control = if Buggy == 9 { 0 } else { 1 }; }
            action Retired when (scenario == 0) { scenario = 4; stored = if Buggy == 4 { 1 } else { 0 }; control = 1; }
            action Damaged when (scenario == 0) { scenario = 5; stored = if Buggy == 5 { 1 } else { 0 }; control = 1; }
            action LostAck when (scenario == 0) { scenario = 6; stored = if Buggy == 6 { 0 } else { 1 }; acknowledged = if Buggy == 8 { 1 } else { 0 }; }
            action Retry when (scenario == 0) { scenario = 7; stored = if Buggy == 7 { 0 } else { 1 }; acknowledged = 1; }
            invariant ReceiptsRequireStorage: acknowledged == 0 || stored == 1;
            invariant WithheldDoesNotWrite: 3 > scenario || scenario > 5 || stored == 0;
            invariant AvailableRepairPersists: scenario == 0 || (scenario > 2 && scenario <= 5) || stored == 1;
            invariant ControlSurvivesUpgrade: scenario == 0 || scenario > 5 || control == 1;
            invariant LostReceiptIsNotAcknowledged: 6 > scenario || scenario > 6 || acknowledged == 0;
        }
    }
}

/// Real strict-journal restart, corruption, lost custody, crash-tail and fork
/// scenarios. These finite checks do not prove filesystem or crypto primitives.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_durability_model() -> Model {
    aterm_spec::ty_model! {
        DbTableDurability {
            const Buggy = 0;
            var scenario = 0;
            var usable = 0;
            action Restart when (scenario == 0) { scenario = 1; usable = if Buggy == 1 { 0 } else { 1 }; }
            action Truncate when (scenario == 0) { scenario = 2; usable = if Buggy == 2 { 1 } else { 0 }; }
            action LostLog when (scenario == 0) { scenario = 3; usable = if Buggy == 3 { 1 } else { 0 }; }
            action LostClaim when (scenario == 0) { scenario = 4; usable = if Buggy == 4 { 1 } else { 0 }; }
            action ChangedScope when (scenario == 0) { scenario = 5; usable = if Buggy == 5 { 1 } else { 0 }; }
            action TornSuffix when (scenario == 0) { scenario = 6; usable = if Buggy == 6 { 0 } else { 1 }; }
            action UncertainAppend when (scenario == 0) { scenario = 7; usable = if Buggy == 7 { 0 } else { 1 }; }
            action OwnFork when (scenario == 0) { scenario = 8; usable = if Buggy == 8 { 1 } else { 0 }; }
            action CopiedStore when (scenario == 0) { scenario = 9; usable = if Buggy == 9 { 1 } else { 0 }; }
            action RestoreBackup when (scenario == 0) { scenario = 10; usable = if Buggy == 10 { 1 } else { 0 }; }
            invariant DamagedHistoryWithholdsWriter: usable == 0 || scenario == 1 || scenario == 6 || scenario == 7;
            invariant IntactOrRepairedRemainsUsable: scenario == 0 || (scenario > 1 && 6 > scenario) || scenario == 8 || scenario == 9 || scenario == 10 || usable == 1;
        }
    }
}

/// Bounded envelope admission scenarios executed against real encryption,
/// signatures and secrets-backed sessions; not a proof of cryptographic security.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_envelope_model() -> Model {
    aterm_spec::ty_model! {
        DbTableEnvelope {
            const Buggy = 0;
            var scenario = 0;
            var accepted = 0;
            action Valid when (scenario == 0) { scenario = 1; accepted = if Buggy == 1 { 0 } else { 1 }; }
            action UnknownTable when (scenario == 0) { scenario = 2; accepted = if Buggy == 2 { 1 } else { 0 }; }
            action ForgedReader when (scenario == 0) { scenario = 3; accepted = if Buggy == 3 { 1 } else { 0 }; }
            action WrongEpoch when (scenario == 0) { scenario = 4; accepted = if Buggy == 4 { 1 } else { 0 }; }
            action Tampered when (scenario == 0) { scenario = 5; accepted = if Buggy == 5 { 1 } else { 0 }; }
            action Malformed when (scenario == 0) { scenario = 6; accepted = if Buggy == 6 { 1 } else { 0 }; }
            action ReaderWrites when (scenario == 0) { scenario = 7; accepted = if Buggy == 7 { 1 } else { 0 }; }
            action OldEpoch when (scenario == 0) { scenario = 8; accepted = if Buggy == 8 { 0 } else { 1 }; }
            invariant ExactWriterEvidenceOnly: accepted == 0 || scenario == 1 || scenario == 8;
            invariant ValidHistoryWorks: scenario == 0 || (scenario > 1 && 8 > scenario) || accepted == 1;
        }
    }
}

/// Recipient-bound handoffs release original historical evidence only to a
/// current member, including through a later peer. Not a cryptographic proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn history_handoff_model() -> Model {
    aterm_spec::ty_model! {
        DbHistoryHandoff {
            const Buggy = 0;
            var scenario = 0;
            var exposed = 0;
            action NewMember when (scenario == 0) { scenario = 1; exposed = if Buggy == 1 { 0 } else { 1 }; }
            action Rotated when (scenario == 0) { scenario = 2; exposed = if Buggy == 2 { 0 } else { 1 }; }
            action Onward when (scenario == 0) { scenario = 3; exposed = if Buggy == 3 { 0 } else { 1 }; }
            action WrongRecipient when (scenario == 0) { scenario = 4; exposed = if Buggy == 4 { 1 } else { 0 }; }
            action WrongEpochKey when (scenario == 0) { scenario = 5; exposed = if Buggy == 5 { 1 } else { 0 }; }
            action WrongFileKey when (scenario == 0) { scenario = 6; exposed = if Buggy == 6 { 1 } else { 0 }; }
            action Stale when (scenario == 0) { scenario = 7; exposed = if Buggy == 7 { 1 } else { 0 }; }
            action RetiredRecipient when (scenario == 0) { scenario = 8; exposed = if Buggy == 8 { 1 } else { 0 }; }
            action Forked when (scenario == 0) { scenario = 9; exposed = if Buggy == 9 { 1 } else { 0 }; }
            action DiscardedIssuance when (scenario == 0) { scenario = 10; exposed = if Buggy == 10 { 1 } else { 0 }; }
            action FutureSource when (scenario == 0) { scenario = 11; exposed = if Buggy == 11 { 1 } else { 0 }; }
            invariant OnlyCurrentRecipientReads: exposed == 0 || (scenario > 0 && scenario <= 3);
            invariant HistoricalHandoffWorks: scenario == 0 || scenario > 3 || exposed == 1;
        }
    }
}

/// A historical read key needs current local membership and a selected signed
/// ancestor. It never supplies current write permission. Cases use real rosters.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn historical_access_model() -> Model {
    aterm_spec::ty_model! {
        DbHistoricalAccess {
            const Buggy = 0;
            var scenario = 0;
            var exposed = 0;
            action Current when (scenario == 0) { scenario = 1; exposed = if Buggy == 1 { 0 } else { 1 }; }
            action Rotated when (scenario == 0) { scenario = 2; exposed = if Buggy == 2 { 0 } else { 1 }; }
            action Stale when (scenario == 0) { scenario = 3; exposed = if Buggy == 3 { 1 } else { 0 }; }
            action Missing when (scenario == 0) { scenario = 4; exposed = if Buggy == 4 { 1 } else { 0 }; }
            action ForeignRecipient when (scenario == 0) { scenario = 5; exposed = if Buggy == 5 { 1 } else { 0 }; }
            action Forked when (scenario == 0) { scenario = 6; exposed = if Buggy == 6 { 1 } else { 0 }; }
            action RetiredLocal when (scenario == 0) { scenario = 7; exposed = if Buggy == 7 { 1 } else { 0 }; }
            action Selected when (scenario == 0) { scenario = 8; exposed = if Buggy == 8 { 0 } else { 1 }; }
            action Discarded when (scenario == 0) { scenario = 9; exposed = if Buggy == 9 { 1 } else { 0 }; }
            invariant OnlyCurrentSelectedReleases: exposed == 0 || scenario == 1 || scenario == 2 || scenario == 8;
            invariant SelectedHistoryWorks: scenario == 0 || (scenario > 2 && (8 > scenario || scenario > 8)) || exposed == 1;
        }
    }
}

/// Historical publication validity is causal, while current key access checks
/// every retained publication. Real signed journals exercise these scenarios.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn historical_publication_model() -> Model {
    aterm_spec::ty_model! {
        DbHistoricalPublication {
            const Buggy = 0;
            var scenario = 0;
            var historical = 0;
            var current = 0;
            action Original when (scenario == 0) { scenario = 1; historical = if Buggy == 1 { 0 } else { 1 }; current = 1; }
            action FutureDescendant when (scenario == 0) { scenario = 2; historical = if Buggy == 2 { 0 } else { 1 }; current = if Buggy == 3 { 1 } else { 0 }; }
            action ReusedSource when (scenario == 0) { scenario = 3; historical = if Buggy == 4 { 1 } else { 0 }; current = 0; }
            action FutureSibling when (scenario == 0) { scenario = 4; historical = if Buggy == 5 { 0 } else { 1 }; current = 0; }
            action MergedPastCopy when (scenario == 0) { scenario = 5; historical = if Buggy == 6 { 1 } else { 0 }; current = 0; }
            action DiscardedSource when (scenario == 0) { scenario = 6; historical = if Buggy == 7 { 1 } else { 0 }; current = 0; }
            invariant OriginalEvidenceSurvives: scenario == 0 || scenario == 3 || scenario == 5 || scenario == 6 || historical == 1;
            invariant HistoricalReuseRefused: scenario == 0 || scenario == 1 || scenario == 2 || scenario == 4 || historical == 0;
            invariant CurrentReuseRefused: scenario == 1 || current == 0;
        }
    }
}

/// Only an intact witnessed prefix may retain discarded-source ciphertext.
/// Current projection, transfer and liability admission stay separate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_quarantine_model() -> Model {
    aterm_spec::ty_model! {
        DbTableQuarantine {
            const Buggy = 0;
            var scenario = 0; var opened = 0; var quarantined = 0;
            var writable = 0; var complete = 0; var offered = 0;
            var retained = 0; var received = 0; var visible = 0;
            action Intact when (scenario == 0) {
                scenario = 1; opened = 1; writable = if Buggy == 1 { 0 } else { 1 };
                complete = 1; offered = 1; retained = 1; visible = 1;
            }
            action ForeignDiscarded when (scenario == 0) {
                scenario = 2; opened = 1; quarantined = 1;
                writable = if Buggy == 1 { 0 } else { 1 };
                complete = if Buggy == 2 { 1 } else { 0 };
                offered = if Buggy == 3 { 1 } else { 0 };
                retained = if Buggy == 4 { 0 } else { 1 };
                visible = if Buggy == 11 { 1 } else { 0 };
            }
            action OwnDiscarded when (scenario == 0) {
                scenario = 3; opened = 1; quarantined = 1;
                writable = if Buggy == 5 { 1 } else { 0 }; retained = 1;
            }
            action DiscardedTail when (scenario == 0) {
                scenario = 4; opened = if Buggy == 6 { 1 } else { 0 };
            }
            action ChangedWitness when (scenario == 0) {
                scenario = 5; opened = if Buggy == 7 { 1 } else { 0 };
            }
            action MissingSelectedKey when (scenario == 0) {
                scenario = 6; opened = if Buggy == 8 { 1 } else { 0 };
            }
            action RemoteDiscarded when (scenario == 0) {
                scenario = 7; opened = 1; complete = 1;
                received = if Buggy == 9 { 1 } else { 0 };
            }
            action SelectedDependent when (scenario == 0) {
                scenario = 8; opened = 1; quarantined = 1; retained = 2;
                offered = if Buggy == 12 { 1 } else { 0 };
                visible = if Buggy == 11 { 1 } else { 0 };
                writable = if Buggy == 5 { 1 } else { 0 };
            }
            action ChangedObservation when (scenario == 0) {
                scenario = 9; opened = if Buggy == 10 { 1 } else { 0 };
            }
            action MissingParent when (scenario == 0) {
                scenario = 10; opened = 1; retained = 1; offered = 1; writable = 1;
                complete = if Buggy == 13 { 1 } else { 0 };
            }
            action RepairedParent when (scenario == 0) {
                scenario = 11; opened = 1; retained = 2; offered = 2; visible = 2; writable = 1;
                complete = if Buggy == 14 { 0 } else { 1 };
            }
            invariant HealthyWriterProgress: (scenario > 2 || scenario == 0) || writable == 1;
            invariant UnknownLiabilityWithholds: quarantined == 0 || complete == 0;
            invariant NoQuarantineOffers: (scenario == 0 || scenario == 1 || scenario == 4 || scenario == 5 || scenario == 6 || scenario == 7 || scenario == 9 || scenario > 9) || offered == 0;
            invariant NoQuarantineProjection: (scenario == 0 || scenario == 1 || scenario == 4 || scenario == 5 || scenario == 6 || scenario == 7 || scenario == 9 || scenario > 9) || visible == 0;
            invariant WitnessedBytesRetained: quarantined == 0 || retained > 0;
            invariant TaintedChainBlocked: (scenario == 0 || scenario == 1 || scenario == 2 || scenario == 4 || scenario == 5 || scenario == 6 || scenario == 7 || scenario == 9 || scenario > 9) || writable == 0;
            invariant UnsafeSourcesClosed: (scenario == 0 || scenario == 1 || scenario == 2 || scenario == 3 || scenario == 7 || scenario == 8 || scenario > 9) || opened == 0;
            invariant MissingParentWithholdsLiability: scenario <= 9 || scenario > 10 || complete == 0;
            invariant RepairedParentRestoresLiability: scenario <= 10 || complete == 1;
            invariant NoOpaqueAck: received == 0;
        }
    }
}

/// Selected records cannot launder locally quarantined fork evidence. Cutoff
/// claims must identify the exact writer dot before liabilities are readable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_quarantine_closure_model() -> Model {
    aterm_spec::ty_model! {
        DbTableQuarantineClosure {
            const Buggy = 0;
            var scenario = 0; var offered = 0; var visible = 0;
            var complete = 0; var retained = 0; var forked = 0;
            action TransitiveFork when (scenario == 0) {
                scenario = 1; retained = 3; forked = 1;
                offered = if Buggy == 1 { 1 } else { 0 };
            }
            action GappedSuffix when (scenario == 0) {
                scenario = 2; retained = 2;
                offered = if Buggy == 2 { 1 } else { 0 };
            }
            action ImpliedFork when (scenario == 0) {
                scenario = 3; retained = 2; offered = 0;
                forked = if Buggy == 3 { 0 } else { 1 };
                visible = if Buggy == 3 { 1 } else { 0 };
            }
            action ValidFrontier when (scenario == 0) {
                scenario = 4; retained = 2; offered = 2; visible = 2;
                complete = if Buggy == 4 { 0 } else { 1 };
            }
            action WrongActor when (scenario == 0) {
                scenario = 5; retained = 2; offered = 2; visible = 1;
                complete = if Buggy == 5 { 1 } else { 0 };
            }
            action WrongSequence when (scenario == 0) {
                scenario = 6; retained = 2; offered = 2; visible = 1;
                complete = if Buggy == 6 { 1 } else { 0 };
            }
            invariant TaintedHistoryNotOffered: scenario == 0 || scenario > 2 || offered == 0;
            invariant ImpliedForkRestricts: scenario <= 2 || scenario > 3 || (forked == 1 && visible == 0);
            invariant ValidFrontierReadable: scenario <= 3 || scenario > 4 || complete == 1;
            invariant InvalidFrontierWithholds: scenario <= 4 || complete == 0;
        }
    }
}

/// Native lease admission is exclusive, transfers after close, and is withdrawn
/// if the private inode disappears or is replaced. Worker drainage is tested
/// separately against real TCP; this finite model claims only lease admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn transport_owner_model() -> Model {
    aterm_spec::ty_model! {
        DbTransportOwner {
            const Buggy = 0;
            var scenario = 0;
            var usable = 0;
            action Acquire when (scenario == 0) { scenario = 1; usable = if Buggy == 1 { 0 } else { 1 }; }
            action Contend when (scenario == 0) { scenario = 2; usable = if Buggy == 2 { 1 } else { 0 }; }
            action Transfer when (scenario == 0) { scenario = 3; usable = if Buggy == 3 { 0 } else { 1 }; }
            action Replace when (scenario == 0) { scenario = 4; usable = if Buggy == 4 { 1 } else { 0 }; }
            action Remove when (scenario == 0) { scenario = 5; usable = if Buggy == 5 { 1 } else { 0 }; }
            invariant OnlyExclusiveIntactOwner: usable == 0 || scenario == 1 || scenario == 3;
            invariant OwnerCanStartAndTransfer: scenario == 0 || scenario == 2 || scenario > 3 || usable == 1;
        }
    }
}

/// A returned local writer always has an intact, decryptable key matching its
/// durable public claim. A crash before the first claim can reuse the key;
/// established key loss, corruption or substitution cannot mint a new writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn device_custody_model() -> Model {
    aterm_spec::ty_model! {
        DbDeviceCustody {
            const Buggy = 0;
            var scenario = 0;
            var usable = 0;
            action Fresh when (scenario == 0) { scenario = 1; usable = if Buggy == 1 { 0 } else { 1 }; }
            action Restart when (scenario == 0) { scenario = 2; usable = if Buggy == 2 { 0 } else { 1 }; }
            action UnclaimedKey when (scenario == 0) { scenario = 3; usable = if Buggy == 3 { 0 } else { 1 }; }
            action MissingKey when (scenario == 0) { scenario = 4; usable = if Buggy == 4 { 1 } else { 0 }; }
            action DamagedKey when (scenario == 0) { scenario = 5; usable = if Buggy == 5 { 1 } else { 0 }; }
            action WrongRecipient when (scenario == 0) { scenario = 6; usable = if Buggy == 6 { 1 } else { 0 }; }
            action ChangedClaim when (scenario == 0) { scenario = 7; usable = if Buggy == 7 { 1 } else { 0 }; }
            action MissingIdentity when (scenario == 0) { scenario = 8; usable = if Buggy == 8 { 1 } else { 0 }; }
            invariant InvalidCustodyWithholds: usable == 0 || (scenario > 0 && scenario <= 3);
            invariant IntactKeyRemainsUsable: scenario == 0 || scenario > 3 || usable == 1;
        }
    }
}

/// Once bootstrapped, missing history and changed trust cannot initialize a new
/// network. A vault signer handoff preserves the independent control authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn bootstrap_history_model() -> Model {
    aterm_spec::ty_model! {
        DbBootstrapHistory {
            const Buggy = 0;
            var scenario = 0;
            var opened = 0;
            action FirstBoot when (scenario == 0) { scenario = 1; opened = if Buggy == 1 { 0 } else { 1 }; }
            action Reopen when (scenario == 0) { scenario = 2; opened = if Buggy == 2 { 0 } else { 1 }; }
            action VaultHandoff when (scenario == 0) { scenario = 3; opened = if Buggy == 3 { 0 } else { 1 }; }
            action LostHistory when (scenario == 0) { scenario = 4; opened = if Buggy == 4 { 1 } else { 0 }; }
            action ChangedTrust when (scenario == 0) { scenario = 5; opened = if Buggy == 5 { 1 } else { 0 }; }
            action MissingOuterPin when (scenario == 0) { scenario = 6; opened = if Buggy == 6 { 0 } else { 1 }; }
            action MissingPinAndHistory when (scenario == 0) { scenario = 7; opened = if Buggy == 7 { 1 } else { 0 }; }
            action MissingCommonsDirectory when (scenario == 0) { scenario = 8; opened = if Buggy == 8 { 1 } else { 0 }; }
            action ChangedTrustMissingPin when (scenario == 0) { scenario = 9; opened = if Buggy == 9 { 1 } else { 0 }; }
            action CorruptHistoryClaim when (scenario == 0) { scenario = 10; opened = if Buggy == 10 { 1 } else { 0 }; }
            action LegacyBackfill when (scenario == 0) { scenario = 11; opened = if Buggy == 11 { 0 } else { 1 }; }
            action MissingOuterClaims when (scenario == 0) { scenario = 12; opened = if Buggy == 12 { 0 } else { 1 }; }
            action BusyBootstrap when (scenario == 0) { scenario = 13; opened = if Buggy == 13 { 1 } else { 0 }; }
            action PreparingResume when (scenario == 0) { scenario = 14; opened = if Buggy == 14 { 0 } else { 1 }; }
            action ForeignPreparing when (scenario == 0) { scenario = 15; opened = if Buggy == 15 { 1 } else { 0 }; }
            action LegacyDamagedAnchor when (scenario == 0) { scenario = 16; opened = if Buggy == 16 { 1 } else { 0 }; }
            action OrphanTables when (scenario == 0) { scenario = 17; opened = if Buggy == 17 { 1 } else { 0 }; }
            action WrongIdentity when (scenario == 0) { scenario = 18; opened = if Buggy == 18 { 1 } else { 0 }; }
            action UnclaimedPartial when (scenario == 0) { scenario = 19; opened = if Buggy == 19 { 1 } else { 0 }; }
            action PreparingPinLostLog when (scenario == 0) { scenario = 20; opened = if Buggy == 20 { 1 } else { 0 }; }
            action OrphanHistory when (scenario == 0) { scenario = 21; opened = if Buggy == 21 { 1 } else { 0 }; }
            invariant EstablishedHistoryNeverResets: opened == 0 || (scenario > 0 && scenario <= 3) || scenario == 6 || scenario == 11 || scenario == 12 || scenario == 14;
            invariant ValidBootstrapWorks: scenario == 0 || scenario == 4 || scenario == 5 || scenario == 7 || scenario == 8 || scenario == 9 || scenario == 10 || scenario == 13 || scenario > 14 || opened == 1;
        }
    }
}

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

/// Signed negative evidence survives custody replay without becoming an
/// operation, a refund, or a fresh writer identity. These are bounded scenarios.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn table_conflict_model() -> Model {
    aterm_spec::ty_model! {
        DbTableConflict {
            const Buggy = 0;
            var scenario = 0; var usable = 0; var restricted = 0;
            var acknowledged = 0; var visible = 0; var liability = 0; var writable = 0; var imported = 0;
            action Receive when (scenario == 0) {
                scenario = 1; usable = 1; acknowledged = 1;
                restricted = if Buggy == 1 { 0 } else { 1 };
                imported = if Buggy == 9 { 1 } else { 0 };
                liability = if Buggy == 10 { 0 } else { 1 };
            }
            action Restart when (scenario == 0) {
                scenario = 2; usable = 1; acknowledged = 1; liability = 1;
                restricted = if Buggy == 2 { 0 } else { 1 };
            }
            action Tail when (scenario == 0) {
                scenario = 3; usable = 1; liability = 1;
                restricted = if Buggy == 3 { 0 } else { 1 };
            }
            action Rollback when (scenario == 0) {
                scenario = 4; acknowledged = 1; usable = if Buggy == 4 { 1 } else { 0 };
            }
            action OwnReset when (scenario == 0) {
                scenario = 5; usable = 1; acknowledged = 1; restricted = 1; liability = 1;
                writable = if Buggy == 5 { 1 } else { 0 };
            }
            action WrongScope when (scenario == 0) {
                scenario = 6; usable = 1; acknowledged = if Buggy == 6 { 1 } else { 0 };
            }
            action Forged when (scenario == 0) {
                scenario = 7; usable = 1; acknowledged = if Buggy == 7 { 1 } else { 0 };
            }
            action Retired when (scenario == 0) {
                scenario = 8; usable = 1; acknowledged = 1; liability = 1;
                restricted = if Buggy == 8 { 0 } else { 1 };
            }
            action Replicate when (scenario == 0) {
                scenario = 9; usable = 1; acknowledged = 2;
                restricted = if Buggy == 11 { 0 } else { 2 };
                visible = if Buggy == 13 { 1 } else { 0 };
            }
            action KnownOperation when (scenario == 0) {
                scenario = 10; usable = 1; acknowledged = 1; restricted = 1;
                liability = if Buggy == 12 { 1 } else { 0 };
            }
            action Capacity when (scenario == 0) {
                scenario = 11; usable = 1; acknowledged = 1;
                restricted = if Buggy == 14 { 0 } else { 1 };
            }
            invariant RetainedRestriction: scenario == 0 || scenario == 4 || scenario == 6 || scenario == 7 || restricted > 0;
            invariant AuthenticateBeforeAck: 6 > scenario || scenario > 7 || acknowledged == 0;
            invariant LostWitnessRefuses: scenario <= 3 || scenario > 4 || usable == 0;
            invariant NoWriterReset: writable == 0;
            invariant NoPayloadAdmission: imported == 0 && visible == 0;
            invariant NoFalseLiability: (scenario <= 8) || liability == 0;
            invariant UnrelatedLiabilityAvailable: scenario == 0 || scenario > 1 || liability == 1;
        }
    }
}
