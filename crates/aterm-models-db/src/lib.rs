// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Database protocol models, including explicit unsafe negative controls.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
use aterm_spec::derive::Model;

/// Two authenticated peers repeatedly acquire and release independent inbound
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
            invariant EstablishedHistoryNeverResets: opened == 0 || (scenario > 0 && scenario <= 3);
            invariant ValidBootstrapWorks: scenario == 0 || scenario > 3 || opened == 1;
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
                scenario = 9; usable = 1; acknowledged = 1;
                restricted = if Buggy == 11 { 0 } else { 1 };
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
            invariant RetainedRestriction: scenario == 0 || scenario == 4 || scenario == 6 || scenario == 7 || restricted == 1;
            invariant AuthenticateBeforeAck: 6 > scenario || scenario > 7 || acknowledged == 0;
            invariant LostWitnessRefuses: scenario <= 3 || scenario > 4 || usable == 0;
            invariant NoWriterReset: writable == 0;
            invariant NoPayloadAdmission: imported == 0 && visible == 0;
            invariant NoFalseLiability: (scenario <= 8) || liability == 0;
            invariant UnrelatedLiabilityAvailable: scenario == 0 || scenario > 1 || liability == 1;
        }
    }
}
