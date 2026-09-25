// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WORKSPACE-WIDE PER-INVARIANT NON-VACUITY RATCHET.
//!
//! An invariant no mutant can falsify is a GHOST: it is true by construction, it
//! cannot fail whatever the code does, and the `ty` run that "proves" it reports
//! a proof about nothing. `aterm_spec::verify::uncaught_invariants` isolates each
//! invariant against the `Buggy = 1` family and names the ones nothing catches.
//!
//! **Why this file exists.** That check already existed, and it already caught two
//! real regressions — `press_custody_model` over a self-reported flag, and
//! `selection_custody_model` with five of eight invariants unfalsifiable. But it
//! was a private helper in ONE test file, called for TEN models, while the
//! discharge helpers it guards are called across sixteen. That is the exact shape
//! `ty_drivers_are_armed.rs` was written to end: "Both fixes were correct and
//! neither was structural: they patched the driver that happened to be noticed. A
//! third driver would have been found by a third incident." So this runs the
//! sweep over EVERY model in `xref::model_registry()`, not the ones someone
//! remembered.
//!
//! **This is a ratchet, not a clean bill of health.** The table below is the
//! measured state of the workspace on 2026-09-16, and it is large. Most entries
//! are SPACE guards (`StateBounds`, `ValuesBounded`, `PhaseBounded` and friends),
//! which state the bounds rather than a design claim and are expected to be
//! uncatchable — those are not defects and never will be. The rest are design
//! claims that presently assert nothing, and each is a candidate for a real
//! mutant. The table makes both visible and stops either from growing.
//!
//! **The assertion is TWO-SIDED, which is the whole point.** A NEW uncaught
//! invariant fails the test, and so does an entry that is now CAUGHT but still
//! listed. The second direction is what turns this from a suppression list into a
//! ratchet: you cannot quietly add debt, and you cannot fix an invariant without
//! recording that you did.
//!
//! To update after a deliberate change: run the test, read the diff it prints,
//! and edit the table to match — never the other way round.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Models the interpreter cannot evaluate (a function-valued `Expr` is
/// TLA+-generation only, Tier-0 ty-checked rather than interpreter-evaluable).
/// Listed EXPLICITLY, and asserted to still be ineligible, so "the sweep did not
/// run here" can never be mistaken for "the sweep found nothing here" — which is
/// the same silence the sweep itself exists to refuse.
const INTERPRETER_INELIGIBLE: &[&str] = &[
    "EvictFull",
    "TierResidency",
    "Recording",
    "Coalesce",
    "NativeTabIdentity",
    "NativeDraftJournal",
    "TitleSummaryRuntime",
    "RainbowTerminusAdmission",
    "SettingsPageScroll",
    "PresentRetry",
];

/// `(model, invariants no `Buggy = 1` member falsifies)`, measured 2026-09-16.
const UNCAUGHT: &[(&str, &[&str])] = &[
    ("TerminalModes", &["ModesValid"]),
    ("Ring", &["LenBounded"]),
    ("Cursor", &["CursorBounded"]),
    ("ReadImageSeq", &["SeqIsStaleOrCurrent"]),
    (
        "OperatorEventDelivery",
        &[
            "Bounds",
            "ClaimStateOwnsToken",
            "EscalationOccursAtCap",
            "InDoubtOnlyAfterEscalation",
        ],
    ),
    (
        "OperatorWalActuator",
        &[
            "MutationRequiresDurableIntent",
            "ResultFollowsOneSubmittedMutation",
            "AuthorityStateIsExclusive",
            "DurableOutcomesAreExclusive",
            "ResolutionHasDurableOutcome",
        ],
    ),
    ("OperatorResyncCursor", &["Bounds"]),
    ("OperatorLeadership", &["Bounds"]),
    (
        "OperatorFleetFault",
        &[
            "Bounds",
            "MarkerOwnsEveryBlockedPhase",
            "ClearCommitHasNoAmbiguity",
        ],
    ),
    ("WindowRouting", &["FrontmostLive", "FrontmostAllocated"]),
    ("PressCustody", &["StateBounds"]),
    ("SelectionCustody", &["StateIsBounded"]),
    ("ForwardHandshake", &["WaitingIsBool"]),
    ("TlsBufferedRelay", &["WaitingIsBool"]),
    ("PaneTree", &["TreeNonEmpty"]),
    (
        "ControlConnectionAdmission",
        &[
            "ArrivalsBounded",
            "EveryArrivalAccounted",
            "AcceptedWorkAccounted",
            "CompletedWasAccepted",
        ],
    ),
    (
        "NativeControlRouting",
        &[
            "FrontKindBounded",
            "BareSessionIffFrontTerminal",
            "ExplicitSessionIffLive",
        ],
    ),
    (
        "NativeReopenLedger",
        &[
            "LedgerBounded",
            "NativeLiveBounded",
            "NextIdentityBounded",
            "FailureCountBounded",
        ],
    ),
    (
        "ClosedRecoveryLedgers",
        &[
            "ViewLedgerBounded",
            "TabLedgerBounded",
            "LiveLeavesBounded",
            "FailureCountBounded",
        ],
    ),
    (
        "NativeSettingsSingleton",
        &["RequestingWindowFocused", "OpensBounded"],
    ),
    (
        "NativeSettingsDraftClose",
        &["FlagsBounded", "ResultBounded", "PreservationBounded"],
    ),
    (
        "NativePackagesWorker",
        &[
            "SingleFlightHasOneKind",
            "CommandResultHasOrigin",
            "StateIsBounded",
        ],
    ),
    (
        "NativeMarkdownHistory",
        &["CursorWithinHistory", "EmptyIffNoCursor", "VisitsBounded"],
    ),
    ("NativeMarkdownViewport", &["StepsBounded"]),
    ("NativeEditorViewport", &["ScrollPhaseBounded"]),
    (
        "NativeEditorCommandPalette",
        &["ResultsBounded", "PhaseBounded"],
    ),
    (
        "NativeRecoveryInteraction",
        &["StartsBounded", "CompletionsBounded"],
    ),
    (
        "NativeEditorModal",
        &[
            "ModeBounded",
            "QueryBounded",
            "CaretBounded",
            "AnchorBounded",
            "DocumentEditsBounded",
            "ExitKindBounded",
            "QueryOnlyWhileModal",
        ],
    ),
    (
        "NativeConfigTransaction",
        &[
            "KeysBounded",
            "PatchBaseNotFuture",
            "AcceptedHasRevision",
            "RevisionBounded",
        ],
    ),
    (
        "SeriousModeIntentQueue",
        &[
            "ProjectionTracksLatestIntent",
            "QueueBounded",
            "CompletionBounded",
            "IssuedBounded",
            "ValuesBoolean",
        ],
    ),
    (
        "ConfigFileCommitCas",
        &[
            "IndeterminateDoesNotClaimDurability",
            "OneSerializedCommitOwner",
            "Bounded",
        ],
    ),
    (
        "ConfigCatalogSnapshot",
        &[
            "ViewsNeverAhead",
            "ConsumersUseCompleteSnapshot",
            "RevisionBounded",
        ],
    ),
    (
        "CompositeAccessibilityRoute",
        &["GenerationsBounded", "OwnerDomain"],
    ),
    (
        "NativeDocumentPublication",
        &[
            "SnapshotCurrent",
            "EditorCurrent",
            "AnchorsTransformed",
            "SequenceBounded",
        ],
    ),
    (
        "RestoreManifestSingleUse",
        &[
            "AtMostOneConsumer",
            "ClaimRemovesVisibleName",
            "OwnerBounded",
            "FlagsBounded",
        ],
    ),
    (
        "NativeClosePlan",
        &[
            "NoSilentLoss",
            "FrozenFinalSequence",
            "ClosedHasNoViews",
            "SequenceBounded",
        ],
    ),
    (
        "NativeSaveIntentLatch",
        &[
            "ClosedSequenceIsDurable",
            "DurableNotFuture",
            "TargetNotFuture",
            "RequestedNotFuture",
            "SequenceBounded",
        ],
    ),
    (
        "NativeAsyncDelivery",
        &[
            "AcceptedReducedOnce",
            "DocumentPublishedToEditor",
            "DocumentPublishedToMarkdown",
            "GenerationsBounded",
            "AcceptedBounded",
        ],
    ),
    (
        "TitleSummary",
        &[
            "ObservationRetryIsBoolean",
            "DisabledHasNoObservationRetry",
            "RetiredObservationIsQuiescent",
            "WorkerLaneHasOneStampedJob",
        ],
    ),
    (
        "TitleSummaryObservationScheduler",
        &[
            "ActiveSessionStartsBatch",
            "SelectedSessionIsValid",
            "WorkerSelectionIsValid",
            "Bounds",
        ],
    ),
    (
        "TitleSummaryManagedEndpoint",
        &[
            "EndpointBelongsToOwnedProcess",
            "AutomaticEndpointNeverUsesSharedDefault",
            "ReuseRetainsOwnedEndpoint",
            "Bounds",
        ],
    ),
    (
        "TitleSummarySocketOwnerRetry",
        &[
            "UniqueObservationSucceeds",
            "PermanentErrorsFailClosed",
            "TimeoutConsumesTheBound",
            "RetryBudgetIsBounded",
            "Bounds",
        ],
    ),
    (
        "NativeUpdater",
        &[
            "SingleFlight",
            "GenerationBounded",
            "QuitPolicyDoesNotApply",
        ],
    ),
    (
        "ReleaseDurablePostIntent",
        &[
            "CreateConvergenceRequiresVisibility",
            "UploadRequiresConvergedDraft",
            "UploadConvergenceRequiresVisibility",
            "DurableIntentStateBounded",
        ],
    ),
    (
        "RosterPairRedo",
        &[
            "TargetHalfHasRedoAuthority",
            "StaleSnapshotWritesNothing",
            "StateBounded",
        ],
    ),
    (
        "ReleaseChannelFloor",
        &[
            "FrozenFloorFitsClaim",
            "RuntimeMatchesFrozenJournal",
            "RevalidatedOwnsLease",
            "CompletedReleasesLease",
            "RejectionCannotSilentlyDropLease",
            "AbandonIsExplicitAndTerminal",
            "FloorStateBounds",
        ],
    ),
    (
        "ReleaseJournalPrefix",
        &["CompletionRequiresEveryStep", "JournalPrefixBounds"],
    ),
    (
        "ReleasePublisherFence",
        &["RefusalHasObservedTransportFault", "FenceStateBounds"],
    ),
    (
        "ReleaseKeyEpochTransition",
        &["ConsumedEpochIsClosed", "KeyEpochBounds"],
    ),
    (
        "ReleaseHistoricalRecovery",
        &["CompletionReleasesOwner", "HistoricalRecoveryBounds"],
    ),
    ("ReleasePublishedIdentity", &["PublishedIdentityBounds"]),
    (
        "ReleaseYankSuccessorFirst",
        &["CompleteMeansConverged", "YankStateBounds"],
    ),
    ("ReleaseClaimLanding", &["ClaimStateBounds"]),
    (
        "ReleaseChannelSingleHead",
        &[
            "HistoricalManifestNeverDeleted",
            "HistoricalSignatureNeverDeleted",
            "NominalCrashPreservesRemoteLease",
            "ArchiveStateBounds",
        ],
    ),
    (
        "NativeUpdateAdmission",
        &[
            "ReplacementPreservesForeground",
            "ColdFallbackNeverDropsForeground",
            "UnsafeStateNeverReexecutes",
            "BlockedIsRetryableWithoutReexec",
            "ApplyAtMostOnce",
            "AttemptsBounded",
        ],
    ),
    (
        "NativeUpdateAutoIntent",
        &[
            "UnsuccessfulAttemptRetainsIntent",
            "PhysicalFailureIsManualOnly",
            "AttemptRequiresImportedStage",
            "DeferralsBounded",
            "AcceptedAtMostOnce",
            "AttemptsBounded",
        ],
    ),
    (
        "NativeUpdateHiddenOutputQuiet",
        &[
            "AttemptOnlyAfterAgedQuiet",
            "HiddenSampleRemainsUnacknowledged",
            "Bounds",
        ],
    ),
    (
        "NativeUpdateAttemptIdentity",
        &[
            "ActiveIdentityIsCurrent",
            "RetryUsesFreshIdentity",
            "OneLiveAttemptAuthority",
            "NonceBounded",
            "AbortsBounded",
        ],
    ),
    (
        "NativeUpdateWorkerQueue",
        &[
            "AbstractFifoBoundaryIsBinary",
            "PendingEmptyQueueHasRetryEdge",
            "SettlementIsExplicit",
            "RestartAtMostOnce",
        ],
    ),
    (
        "NativeUpdateStatusReconciliation",
        &[
            "ReadyPreservesPersistedOutcome",
            "HonestTerminalOutcomeIsPreserved",
        ],
    ),
    (
        "TrailAudioLifecycle",
        &[
            "WorkerMailboxIsBounded",
            "DropAccountingIsBounded",
            "RunningOwnsOneDeadline",
            "IdlePauseDisarmsDeadline",
            "StartFailureIsExplicitAndTerminal",
        ],
    ),
    (
        "TrailAudioStartLatency",
        &[
            "BufferOwnershipConserved",
            "IdleIsCallbackAndWakeFree",
            "PhaseBounded",
        ],
    ),
    (
        "AsymmetricPadLayout",
        &[
            "BottomAbsorbsFreedPixels",
            "GridOriginTracksTopAndHead",
            "IdenticalLayoutMayReuseCache",
            "CacheDecisionIsTotal",
        ],
    ),
    (
        "VisiblePadCrop",
        &["TopIsClamped", "RawTransportConservesTwoPads"],
    ),
    ("FocusModifierCache", &["StateBounds"]),
    (
        "InputReleasePairing",
        &[
            "LiteralInputRetainsSilentReleaseOwnership",
            "LocalRepeatRetainsSilentReleaseOwnership",
            "StateBounds",
        ],
    ),
    (
        "TabStopHandoff",
        &[
            "AdmissionIsCoveringAndBounded",
            "InvalidProjectionIsNeverAdmitted",
        ],
    ),
    (
        "ScrollbackMaintenanceLane",
        &[
            "MutationRequiresMemoryPressure",
            "MutationRequiresCompletedPressureTrim",
        ],
    ),
    (
        "TopAnchoredScrollHistory",
        &["FixedFooterIsPreserved", "StateIsBounded"],
    ),
    (
        "KittySingDetector",
        &["DriveMatchesLifecycle", "CountBounded", "PhaseBounded"],
    ),
    ("PetStrokeDetector", &["StrokeStateBounded"]),
    ("ConsoleLifeEpisode", &["StateBounded"]),
    ("ConsoleResidentHandoff", &["StateBounded"]),
    (
        "CursorCatEarnFloor",
        &["ActiveRequiresSinging", "RunBounded", "FlagsBounded"],
    ),
    ("CursorCatCurseWince", &["HiddenCueNeverSummons"]),
    ("ReducedMotionCompanionHandoff", &["StateBounded"]),
    (
        "CursorCatMotionPulseRouting",
        &["DeliveryIsClassifiedAndAtMostOnce", "StateBounded"],
    ),
    ("CursorHintLicense", &["StateBounded"]),
    ("RainbowTypedContinuity", &["Bounded"]),
    ("SameCaretTypedEcho", &["Bounded"]),
    ("UnknownInsertOrphanKey", &["Bounded"]),
    ("EchoLedgerBridge", &["StateBounded"]),
    (
        "CursorViewportLifecycle",
        &[
            "HiddenPetLifecycleProgresses",
            "CursorViewportValuesBounded",
        ],
    ),
    (
        "CursorCompanionOwnerLifecycle",
        &[
            "PresentationPinsPreserveTheSighting",
            "DurableIdentitySurvives",
            "CompanionOwnerValuesBounded",
        ],
    ),
    ("ComposedSyncHold", &["ComposedSyncValuesBounded"]),
    (
        "SyncReopenVisibility",
        &[
            "CleanReopenMayPresentCompletedBoundary",
            "SyncReopenValuesBounded",
        ],
    ),
    (
        "RainbowJumpBurstLifecycle",
        &[
            "ResidentBounded",
            "GhostBounded",
            "TotalBounded",
            "GhostIdentityBounded",
            "WakeMatchesResidents",
            "IssuedBounded",
            "WakeBounded",
        ],
    ),
    (
        "NativeUpdateOverlapHandoff",
        &[
            "ParentExitRequiresCommitOrLegacyAck",
            "GroupSignalEliminatesLiveDescendants",
        ],
    ),
    (
        "NativeUpdateDiskTransaction",
        &[
            "LegacyRefusalPreservesRecoveryAuthority",
            "ModernReadyRecoveryRequiresReadyAndIsExact",
            "ReceiptBindsExactNewIdentity",
            "FailedSwapNeverReplacesOld",
            "FailedRollbackPreservesRecoveryAuthority",
            "ExecFailureCannotGcBeforeRestore",
            "CrashLoopRestoreUsesExactOld",
            "RollbackGcRequiresRestoreAndDisarm",
            "CrashBudgetBounded",
        ],
    ),
    (
        "ExactProfanityCompletion",
        &[
            "ActiveUsesCanonicalIdentity",
            "HarmlessAndSettledAreInactive",
            "CompletionCreatesExactlyOneEpisode",
        ],
    ),
    ("SnapshotGenerationCommit", &["Bounds"]),
    (
        "VideoRecordingLifecycle",
        &[
            "Bounds",
            "ModeMatchesRecordingPhase",
            "TapOnlyOnGlass",
            "OffscreenTimerExact",
            "LateCancellationCannotRevoke",
        ],
    ),
    (
        "ExactInstanceRetention",
        &["Bounds", "MissingAloneUsesPidFallback"],
    ),
    (
        "AnchoredArtifactTransaction",
        &[
            "Bounds",
            "ActiveTransactionIsPinned",
            "PathIdentityTracksAncestor",
            "OperationRequiresPinnedObject",
            "CompletedReplyWasValidated",
            "FailedReplyCertifiesNothing",
        ],
    ),
    (
        "ArtifactReplyPublication",
        &[
            "Bounds",
            "ChallengeRequiresCompleteWire",
            "AbortReleaseRemovesUncommittedArtifact",
        ],
    ),
    (
        "ArtifactHandoffCapacity",
        &["CountMatchesCharges", "UnitsMatchCharges", "LiveOwnsCharge"],
    ),
    ("VideoBatchPublicationDurability", &["Bounds"]),
    (
        "ArtifactReaderLease",
        &[
            "Bounds",
            "MaintenanceRequiresArm",
            "FinishedSweepReopensIdle",
        ],
    ),
    ("CaptureAfterPresent", &["AttemptsBounded", "ValuesBounded"]),
    ("NativeCaptureSource", &["ValuesBounded"]),
    (
        "PresentedFrameTap",
        &[
            "ReservedPhaseRequiresAcceptedCopy",
            "TerminalPhaseHasResult",
            "ResultOnlyAtTerminal",
            "ValuesBounded",
        ],
    ),
    (
        "VideoTapSlot",
        &[
            "DropCountBounded",
            "EvictionMatchesOverflow",
            "ValuesBounded",
        ],
    ),
    (
        "HdrReconfigureRetag",
        &["AwaitingUpgradeIsSdr", "ValuesBounded"],
    ),
    (
        "LayoutCoordinateReset",
        &["CoordinateBounded", "ValuesBounded"],
    ),
    (
        "SemanticPrewarmGeneration",
        &[
            "QueueContainsOnlyCurrent",
            "GenerationsBounded",
            "FlagsBounded",
        ],
    ),
    (
        "SemanticPrewarmHandshake",
        &[
            "CurrentFailureFailsClosed",
            "CacheOnlySupersededReady",
            "InputsBounded",
            "OutputsBounded",
        ],
    ),
    (
        "SemanticPrewarmRequestSwap",
        &["InputsWellFormed", "FlagsBounded"],
    ),
    ("SurfaceCoverage", &["FrameFitsSurface"]),
    ("StartupPhasePublication", &["PhaseBounded"]),
    ("GpuLossRoute", &["RouteRange"]),
    (
        "GpuLossRecovery",
        &[
            "Bounds",
            "ExhaustedFailureIsParked",
            "DeliveredRetryHasNoDeadline",
            "FailedFallbackIsDiagnosed",
            "ReadyCpuOwnsRedrawUntilPresent",
            "CpuPresentWasReady",
            "CpuPresentCompletesFailure",
        ],
    ),
    (
        "RecoveryRedraw",
        &["Bounds", "RecoveryStimulusRequestsRedraw"],
    ),
    ("PredictiveEchoVisibility", &["Bounds"]),
    ("OutputEchoReceiptPublication", &["StateBounded"]),
    // The 0..2 queue counts define this bounded model's state space; the
    // cross-session design claim is DecisionReadsOnlySelectedSink, which the
    // process-wide ACTIVE mutant falsifies.
    ("PasteOrderSinkIsolation", &["PendingIsBounded"]),
    (
        "StreamingSearch",
        &[
            "MemoryBounded",
            "TotalMatchesConsistent",
            "ScanProgressConsistent",
        ],
    ),
    (
        "BudgetedSearchResume",
        &[
            "LifecycleShape",
            "CursorMatchesSearchId",
            "DeliveryShape",
            "IdentityIsLatest",
            "ValuesBounded",
        ],
    ),
];

#[test]
fn no_model_grows_a_ghost_invariant() {
    let models = aterm_spec::xref::model_registry();

    // Silence the interpreter's panic output for the eligibility probe below;
    // the panics are expected and are reported by this test, not by libtest.
    std::panic::set_hook(Box::new(|_| {}));
    let mut measured: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut ineligible: Vec<&str> = Vec::new();
    for m in &models {
        match catch_unwind(AssertUnwindSafe(|| {
            aterm_spec::verify::uncaught_invariants(m)
        })) {
            Ok(names) if !names.is_empty() => measured.push((m.name, names)),
            Ok(_) => {}
            Err(_) => ineligible.push(m.name),
        }
    }
    let _ = std::panic::take_hook();

    assert_eq!(
        ineligible, INTERPRETER_INELIGIBLE,
        "the set of models the interpreter cannot evaluate changed. A model that \
         became evaluable must be REMOVED from `INTERPRETER_INELIGIBLE` so the \
         sweep starts covering it; a model that became ineligible is a regression \
         in the model, not a licence to stop checking it."
    );

    let expected: std::collections::BTreeMap<&str, Vec<&str>> = UNCAUGHT
        .iter()
        .map(|(model, invs)| (*model, invs.to_vec()))
        .collect();
    let actual: std::collections::BTreeMap<&str, Vec<&str>> = measured.into_iter().collect();

    let mut grew: Vec<String> = Vec::new();
    let mut fixed: Vec<String> = Vec::new();
    for (model, invs) in &actual {
        let known = expected.get(model).cloned().unwrap_or_default();
        for inv in invs {
            if !known.contains(inv) {
                grew.push(format!("{model}::{inv}"));
            }
        }
    }
    for (model, invs) in &expected {
        let now = actual.get(model).cloned().unwrap_or_default();
        for inv in invs {
            if !now.contains(inv) {
                fixed.push(format!("{model}::{inv}"));
            }
        }
    }

    assert!(
        grew.is_empty(),
        "NEW ghost invariant(s) — no `Buggy = 1` member falsifies these, so they \
         assert nothing about the code and any `ty` proof of them proves nothing:\n  {}\n\
         Give each one a mutant that violates it, or (if it states the SPACE rather \
         than a design claim) add it to the table with that reason.",
        grew.join("\n  ")
    );
    assert!(
        fixed.is_empty(),
        "these invariant(s) are now CAUGHT by a mutant but are still listed as \
         uncaught:\n  {}\n\
         Remove them from `UNCAUGHT`. The ratchet is two-sided on purpose: a fix \
         that is not recorded is a fix the next regression can silently undo.",
        fixed.join("\n  ")
    );
}
