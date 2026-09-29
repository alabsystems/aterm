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
//! **An uncaught invariant is one of two things, and only one is allowed.** A
//! SPACE guard (`StateBounds`, `ValuesBounded`, `PhaseBounded` and friends)
//! states the bounds the interpreter and `ty` are asked to walk, not a design
//! claim; it is expected to be uncatchable and is listed in `SPACE_GUARDS`.
//! Anything else uncaught is a GHOST — a design claim that asserts nothing — and
//! fails this test: give it a real mutant (a `Buggy` branch reproducing a
//! plausible defect the invariant catches) or, if it only restates a guard or
//! another invariant, delete it. There is no third list. Ghosts measured on
//! 2026-09-16 were carried as debt in an `UNCAUGHT` table while they were paid
//! down; the last of them went on 2026-09-26, and the table went with them, so
//! nothing can be parked there again.
//!
//! **The assertion is TWO-SIDED, which is the whole point.** The uncaught set
//! must EQUAL `SPACE_GUARDS`: a new ghost fails, and so does a listed guard some
//! mutant now falsifies — that "space guard" was a design claim all along and
//! must move out. `INTERPRETER_INELIGIBLE` is held to the same equality, so a
//! model the sweep cannot evaluate is named, never silently skipped.
//!
//! **A space guard is checked for its SHAPE, not taken on trust.** Filing a
//! design claim under `SPACE_GUARDS` would launder a ghost as a bound, so
//! `every_space_guard_is_a_range_bound` requires each listed invariant to be a
//! conjunction of range atoms — ONE state variable compared (`<=`, `>`) against
//! an expression over literals and constants — that leaves every variable it
//! bounds more than one value. A relation between two variables
//! (`published <= accepted`) is a claim about the design, even when it sits
//! inside a `StateBounded`; so is a pin (`x <= 0`, or a bound over the `Buggy`
//! dial, which is no constant). Each gets its own name and its own mutant.
//!
//! To update after a deliberate change: run the test, read what it prints, and
//! fix the model (or, for a genuine bound, `SPACE_GUARDS`) — never the other way
//! round.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use aterm_spec::derive::Expr;

/// Models the interpreter cannot evaluate (a function-valued `Expr` is
/// TLA+-generation only, Tier-0 ty-checked rather than interpreter-evaluable).
/// Listed EXPLICITLY, and asserted to still be ineligible, so "the sweep did not
/// run here" can never be mistaken for "the sweep found nothing here" — which is
/// the same silence the sweep itself exists to refuse.
const INTERPRETER_INELIGIBLE: &[&str] = &[
    "EvictFull",
    "TierResidency",
    "Recording",
    "NativeTabIdentity",
    "NativeDraftJournal",
    "TitleSummaryRuntime",
    "SettingsPageScroll",
    "PresentRetry",
];

/// `(model, invariants that state the SPACE, not the design)` — uncaught by
/// every `Buggy = 1` member and expected to stay so. Asserted still-uncaught,
/// like `INTERPRETER_INELIGIBLE`: a guard a mutant starts to falsify is a design
/// claim and moves out of this list.
const SPACE_GUARDS: &[(&str, &[&str])] = &[
    ("OperatorResyncCursor", &["Bounds"]),
    ("OperatorLeadership", &["Bounds"]),
    ("PressCustody", &["StateBounds"]),
    ("SelectionCustody", &["StateIsBounded"]),
    // Both `props::no_wedge` instances: `waiting <= 1` bounds a flag, and the
    // models' design claim is the separate deadlock-freedom obligation.
    ("ForwardHandshake", &["WaitingIsBool"]),
    ("TlsBufferedRelay", &["WaitingIsBool"]),
    ("NativeMarkdownViewport", &["StepsBounded"]),
    ("NativeEditorViewport", &["ScrollPhaseBounded"]),
    // `active_work` projects `Option<UpdaterWorkTicket>::is_some()`: a second
    // worker is unrepresentable, so `SingleFlight` bounds a flag. The join law is
    // bound at Tier-1 on the real `CheckStart::Joined`.
    ("NativeUpdater", &["SingleFlight", "GenerationBounded"]),
    ("ReleasePublishedIdentity", &["PublishedIdentityBounds"]),
    ("ReleaseClaimLanding", &["ClaimStateBounds"]),
    ("FocusModifierCache", &["StateBounds"]),
    ("PetStrokeDetector", &["StrokeStateBounded"]),
    ("ConsoleLifeEpisode", &["StateBounded"]),
    ("ConsoleResidentHandoff", &["StateBounded"]),
    ("ReducedMotionCompanionHandoff", &["StateBounded"]),
    ("CursorHintLicense", &["StateBounded"]),
    ("RainbowTypedContinuity", &["Bounded"]),
    ("SameCaretTypedEcho", &["Bounded"]),
    ("UnknownInsertOrphanKey", &["Bounded"]),
    ("EchoLedgerBridge", &["StateBounded"]),
    ("ComposedSyncHold", &["ComposedSyncValuesBounded"]),
    ("SnapshotGenerationCommit", &["Bounds"]),
    ("VideoBatchPublicationDurability", &["Bounds"]),
    ("CaptureAfterPresent", &["ValuesBounded"]),
    ("NativeCaptureSource", &["ValuesBounded"]),
    ("SurfaceCoverage", &["FrameFitsSurface"]),
    ("StartupPhasePublication", &["PhaseBounded"]),
    ("GpuLossRoute", &["RouteRange"]),
    ("PredictiveEchoVisibility", &["Bounds"]),
    ("OutputEchoReceiptPublication", &["StateBounded"]),
    // The 0..2 queue counts define this bounded model's state space; the
    // cross-session design claim is DecisionReadsOnlySelectedSink, which the
    // process-wide ACTIVE mutant falsifies.
    ("PasteOrderSinkIsolation", &["PendingIsBounded"]),
    // The displacement box the resize ledger's render verdict is explored in;
    // every verdict claim (`RiskIsFlagged`, `NoSilentPass`, `NoFalseVerdict`)
    // falls to the size-difference judge, and `QuietFlapIsIdentity` to the
    // historical append-only grow, all in the one `Buggy = 1` member.
    ("ResizeRender", &["Bounded"]),
    ("RainbowLandingPool", &["StateBounds"]),
    ("OperatorEventDelivery", &["Bounds"]),
    ("OperatorFleetFault", &["Bounds"]),
    ("ControlConnectionAdmission", &["ArrivalsBounded"]),
    ("NativeControlRouting", &["FrontKindBounded"]),
    (
        "NativeReopenLedger",
        &[
            "NativeLiveBounded",
            "NextIdentityBounded",
            "FailureCountBounded",
        ],
    ),
    (
        "ClosedRecoveryLedgers",
        &["LiveLeavesBounded", "FailureCountBounded"],
    ),
    ("NativeSettingsSingleton", &["OpensBounded"]),
    (
        "NativeSettingsDraftClose",
        &["FlagsBounded", "ResultBounded", "PreservationBounded"],
    ),
    ("NativePackagesWorker", &["StateIsBounded"]),
    ("NativeMarkdownHistory", &["VisitsBounded"]),
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
            "DocumentEditsBounded",
            "ExitKindBounded",
        ],
    ),
    // The native-app config, document, async-delivery and Smart Title models:
    // every design claim beside these bounds carries its own mutant.
    (
        "NativeConfigTransaction",
        &["KeysBounded", "RevisionBounded"],
    ),
    (
        "SeriousModeIntentQueue",
        &["QueueBounded", "IssuedBounded", "ValuesBoolean"],
    ),
    ("ConfigFileCommitCas", &["Bounded"]),
    ("ConfigCatalogSnapshot", &["RevisionBounded"]),
    (
        "CompositeAccessibilityRoute",
        &["GenerationsBounded", "OwnerDomain"],
    ),
    ("NativeDocumentPublication", &["SequenceBounded"]),
    (
        "RestoreManifestSingleUse",
        &["OwnerBounded", "FlagsBounded"],
    ),
    ("NativeClosePlan", &["SequenceBounded"]),
    ("NativeSaveIntentLatch", &["SequenceBounded"]),
    ("NativeAsyncDelivery", &["GenerationsBounded"]),
    ("TitleSummary", &["ObservationRetryIsBoolean"]),
    (
        "TitleSummaryObservationScheduler",
        &["SelectedSessionIsValid", "WorkerSelectionIsValid", "Bounds"],
    ),
    ("TitleSummaryManagedEndpoint", &["Bounds"]),
    (
        "TitleSummarySocketOwnerRetry",
        &["RetryBudgetIsBounded", "Bounds"],
    ),
    // The release/updater batch (2026-09-25): each bound below is the model's
    // explored box and nothing more; every design claim beside it has its own
    // mutant, and the laws that only restated a guard or another law are gone.
    ("ReleaseDurablePostIntent", &["DurableIntentStateBounded"]),
    ("RosterPairRedo", &["StateBounded"]),
    ("ReleaseChannelFloor", &["FloorStateBounds"]),
    ("ReleaseJournalPrefix", &["JournalPrefixBounds"]),
    ("ReleasePublisherFence", &["FenceStateBounds"]),
    ("ReleaseHistoricalRecovery", &["HistoricalRecoveryBounds"]),
    ("ReleaseYankSuccessorFirst", &["YankStateBounds"]),
    ("NativeUpdateAdmission", &["AttemptsBounded"]),
    (
        "NativeUpdateAutoIntent",
        &["DeferralsBounded", "AttemptsBounded"],
    ),
    ("NativeUpdateHiddenOutputQuiet", &["Bounds"]),
    (
        "NativeUpdateAttemptIdentity",
        &["NonceBounded", "AbortsBounded"],
    ),
    ("NativeUpdateDiskTransaction", &["CrashBudgetBounded"]),
    // The saturating drop counter. The 64-slot FIFO's bound is a design claim
    // (the one-token `mpsc::channel()` slip breaks it), not space.
    ("TrailAudioLifecycle", &["DropAccountingIsBounded"]),
    ("TrailAudioStartLatency", &["PhaseBounded"]),
    ("InputReleasePairing", &["StateBounds"]),
    ("TopAnchoredScrollHistory", &["StateIsBounded"]),
    ("KittySingDetector", &["CountBounded", "PhaseBounded"]),
    ("CursorCatEarnFloor", &["RunBounded", "FlagsBounded"]),
    ("CursorCatMotionPulseRouting", &["StateBounded"]),
    ("CursorViewportLifecycle", &["CursorViewportValuesBounded"]),
    (
        "CursorCompanionOwnerLifecycle",
        &["CompanionOwnerValuesBounded"],
    ),
    ("SyncReopenVisibility", &["SyncReopenValuesBounded"]),
    (
        "LayoutCoordinateReset",
        &["CoordinateBounded", "ValuesBounded"],
    ),
    ("BudgetedSearchResume", &["ValuesBounded"]),
    ("VideoRecordingLifecycle", &["Bounds"]),
    ("ExactInstanceRetention", &["Bounds"]),
    ("AnchoredArtifactTransaction", &["Bounds"]),
    ("ArtifactReplyPublication", &["Bounds"]),
    ("ArtifactReaderLease", &["Bounds"]),
    ("PresentedFrameTap", &["ValuesBounded"]),
    // `MaxDrops` truncates the shipping take's unbounded `dropped` counter.
    ("VideoTapSlot", &["DropCountBounded", "ValuesBounded"]),
    ("HdrReconfigureRetag", &["ValuesBounded"]),
    (
        "SemanticPrewarmGeneration",
        &["GenerationsBounded", "FlagsBounded"],
    ),
    (
        "SemanticPrewarmHandshake",
        &["InputsBounded", "OutputsBounded"],
    ),
    ("SemanticPrewarmRequestSwap", &["FlagsBounded"]),
    ("GpuLossRecovery", &["Bounds"]),
    ("RecoveryRedraw", &["Bounds"]),
    // `passes` is the bounded retry phase (first, second, later); the
    // scheduling claim is NoPrematureSameBuildPass, which Buggy falsifies.
    ("AtpkgSessionIndexRetry", &["BoundedRetry"]),
    // The relaunch count bounds the explored box; the brake's policy is
    // `KeeperRelaunchBrake`, whose `StreakBounded` is a design claim with a
    // mutant (docs/DESIGN-pty-keeper-2026-09-26.md §6.1/§6.2).
    ("PtyKeeperCustody", &["RelaunchSpace"]),
];

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs one interpreter probe, catching its panic: `Err(())` is the probe's
/// verdict that the model does not fit, which the caller reports. The panic's
/// own output is silenced on THIS thread only. The hook is process-wide and the
/// sweeps run as parallel tests, so a hook swapped per sweep would let one
/// sweep's restore print the other's expected panics, or one sweep's silence
/// swallow another test's assertion message.
fn quietly<T>(probe: impl FnOnce() -> T) -> Result<T, ()> {
    static FILTER: Once = Once::new();
    FILTER.call_once(|| {
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.with(Cell::get) {
                report(info);
            }
        }));
    });
    QUIET.with(|quiet| quiet.set(true));
    let verdict = catch_unwind(AssertUnwindSafe(probe)).map_err(|_| ());
    QUIET.with(|quiet| quiet.set(false));
    verdict
}

#[test]
fn no_model_grows_a_ghost_invariant() {
    let models = aterm_spec::xref::model_registry();

    let mut actual: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut ineligible: Vec<&str> = Vec::new();
    for m in &models {
        match quietly(|| aterm_spec::verify::uncaught_invariants(m)) {
            Ok(names) => actual.extend(names.into_iter().map(|inv| (m.name, inv))),
            Err(()) => ineligible.push(m.name),
        }
    }

    assert_eq!(
        ineligible, INTERPRETER_INELIGIBLE,
        "the set of models the interpreter cannot evaluate changed. A model that \
         became evaluable must be REMOVED from `INTERPRETER_INELIGIBLE` so the \
         sweep starts covering it; a model that became ineligible is a regression \
         in the model, not a licence to stop checking it."
    );

    let guards: BTreeSet<(&str, &str)> = SPACE_GUARDS
        .iter()
        .flat_map(|(model, invs)| invs.iter().map(move |inv| (*model, *inv)))
        .collect();
    let show = |set: Vec<&(&str, &str)>| -> String {
        set.iter()
            .map(|(model, inv)| format!("{model}::{inv}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    };

    let ghosts: Vec<_> = actual.difference(&guards).collect();
    let caught_guards: Vec<_> = guards.difference(&actual).collect();

    assert!(
        ghosts.is_empty(),
        "GHOST invariant(s) — no `Buggy = 1` member falsifies these, so they \
         assert nothing about the code and any `ty` proof of them proves nothing:\n  {}\n\
         Give each one a mutant that violates it, delete it if it only restates a \
         guard or another invariant, or (if it states the SPACE rather than a design \
         claim) add it to `SPACE_GUARDS`.",
        show(ghosts)
    );
    assert!(
        caught_guards.is_empty(),
        "these `SPACE_GUARDS` entries are CAUGHT by a mutant (or no longer exist):\n  {}\n\
         A bound some `Buggy = 1` member falsifies is a design claim — remove it from \
         `SPACE_GUARDS`.",
        show(caught_guards)
    );
}

/// THE LIVENESS HALF OF THE RATCHET. An invariant no mutant breaks is a ghost,
/// and so is a liveness property: `[]<>goal` that holds whatever the lane does
/// proves nothing about the lane. Every obligation in
/// `xref::liveness_registry()` goes through `verify::audit_liveness` — proved at
/// the committed config, every stated fairness assumption load-bearing, the
/// `Buggy = 1` baseline clean, and each named mutant breaking it alone — and is
/// stated over a model that is itself registered, byte-for-byte, so a liveness
/// obligation cannot hang off a private copy of a machine the rest of the
/// workspace checks differently.
///
/// The per-model test (`derived_ring_ty.rs`) runs the same audit and adds the
/// `ty` tier; this sweep exists so a registered obligation cannot lose its
/// catches while nobody runs that one test.
#[test]
fn every_liveness_obligation_is_proven_minimal_and_caught() {
    let models: BTreeMap<&str, String> = aterm_spec::xref::model_registry()
        .iter()
        .map(|m| (m.name, m.to_tla()))
        .collect();
    let obligations = aterm_spec::xref::liveness_registry();
    assert!(
        !obligations.is_empty(),
        "the liveness registry is empty: Law 1 of the apply ladder is registered there"
    );
    let mut failures: Vec<String> = Vec::new();
    for (m, live) in &obligations {
        match models.get(m.name) {
            None => failures.push(format!(
                "{} liveness `{}`: the model is not in `xref::model_registry()`",
                m.name, live.name
            )),
            Some(registered) if *registered != m.to_tla() => failures.push(format!(
                "{} liveness `{}`: stated over a model that differs from the registered one",
                m.name, live.name
            )),
            Some(_) => {}
        }
        if let Err(why) = aterm_spec::verify::audit_liveness(m, live) {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "liveness obligations that do not hold, are not minimal, or are not caught:\n  {}",
        failures.join("\n  ")
    );
}

/// Every evaluable model's `Buggy = 1` space, walked with every invariant
/// stripped, fits the interpreter's state budget. `uncaught_invariants` walks
/// exactly that space for an invariant no mutant breaks, so where it does not fit
/// the sweep cannot NAME a ghost: it panics, and the model reads as
/// `INTERPRETER_INELIGIBLE` instead. A mutant that can repeat its slip forever
/// (each spawn/close cycle stranding one more registry entry, say) is how a model
/// lands here, and the fix is a mutant that fires only from a sound state, as
/// `props::lifecycle_no_leak`'s does.
#[test]
fn every_buggy_space_fits_the_interpreter() {
    let mut unbounded: Vec<&str> = Vec::new();
    for m in aterm_spec::xref::model_registry() {
        if INTERPRETER_INELIGIBLE.contains(&m.name) {
            continue;
        }
        let mut space = aterm_spec::interp::with_buggy(&m, 1);
        space.invariants.clear();
        if quietly(|| aterm_spec::interp::bmc(&space)).is_err() {
            unbounded.push(m.name);
        }
    }
    assert!(
        unbounded.is_empty(),
        "these models' Buggy=1 space exceeds the interpreter's budget, so the ghost \
         sweep cannot name their uncaught invariants:\n  {}\n\
         Each has a mutant that can fire forever: guard it to fire only from a state \
         the correct model can reach.",
        unbounded.join("\n  ")
    );
}

/// The value of `e` under the model's constants, or `None` when it mentions a
/// state variable. `Buggy` does not count as a constant: it is the mutant dial,
/// and a bound over it (`x <= Buggy`) pins the variable at the committed config.
fn constant_value(e: &Expr, consts: &[(&str, i64)]) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::ConstRef(name) if *name != "Buggy" => consts
            .iter()
            .find_map(|&(constant, value)| (constant == *name).then_some(value)),
        Expr::Add(a, b) => Some(constant_value(a, consts)? + constant_value(b, consts)?),
        Expr::Sub(a, b) => Some(constant_value(a, consts)? - constant_value(b, consts)?),
        _ => None,
    }
}

/// A range atom: ONE state variable compared against a constant expression,
/// either way round, as `(variable, lowest allowed, highest allowed)`.
fn range_atom(
    e: &Expr,
    consts: &[(&str, i64)],
) -> Option<(&'static str, Option<i64>, Option<i64>)> {
    let k = |e: &Expr| constant_value(e, consts);
    match e {
        Expr::Le(a, b) => match (&**a, &**b) {
            (Expr::Var(x), bound) => Some((*x, None, Some(k(bound)?))),
            (bound, Expr::Var(x)) => Some((*x, Some(k(bound)?), None)),
            _ => None,
        },
        Expr::Gt(a, b) => match (&**a, &**b) {
            (Expr::Var(x), bound) => Some((*x, Some(k(bound)? + 1), None)),
            (bound, Expr::Var(x)) => Some((*x, None, Some(k(bound)? - 1))),
            _ => None,
        },
        _ => None,
    }
}

/// Why `e`, a space guard's body, is not a range bound: each conjunct that is
/// not a range atom, and each variable its atoms PIN to a single value (or to
/// none). Model variables count up from zero, so an upper bound of zero
/// (`x <= 0`, `1 > x`) or a lower bound meeting the upper one (`x > 0 && x <= 1`)
/// says "this never moves": a claim about the design, not the extent of the space.
fn not_range_bounds(e: &Expr, consts: &[(&str, i64)]) -> Vec<String> {
    fn conjuncts<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
        match e {
            Expr::And(a, b) => {
                conjuncts(a, out);
                conjuncts(b, out);
            }
            atom => out.push(atom),
        }
    }
    let mut atoms = Vec::new();
    conjuncts(e, &mut atoms);
    let mut bad = Vec::new();
    let mut extent: BTreeMap<&str, (i64, Option<i64>)> = BTreeMap::new();
    for atom in atoms {
        let Some((x, low, high)) = range_atom(atom, consts) else {
            bad.push(atom.to_tla());
            continue;
        };
        let (lo, hi) = extent.entry(x).or_insert((0, None));
        *lo = (*lo).max(low.unwrap_or(0));
        *hi = match (*hi, high) {
            (Some(h), Some(n)) => Some(h.min(n)),
            (h, n) => h.or(n),
        };
    }
    for (x, (lo, hi)) in extent {
        if let Some(hi) = hi
            && hi <= lo
        {
            bad.push(format!("`{x}` is pinned to {lo}..={hi}"));
        }
    }
    bad
}

#[test]
fn every_space_guard_is_a_range_bound() {
    let models = aterm_spec::xref::model_registry();
    let mut laundered: Vec<String> = Vec::new();
    for (model, guards) in SPACE_GUARDS {
        let m = models
            .iter()
            .find(|m| m.name == *model)
            .unwrap_or_else(|| panic!("`SPACE_GUARDS` names `{model}`, which is not registered"));
        for guard in *guards {
            let inv = m
                .invariants
                .iter()
                .find(|inv| inv.name == *guard)
                .unwrap_or_else(|| panic!("`{model}` has no invariant `{guard}`"));
            for atom in not_range_bounds(&inv.expr, &m.consts) {
                laundered.push(format!("{model}::{guard}: {atom}"));
            }
        }
    }
    assert!(
        laundered.is_empty(),
        "these `SPACE_GUARDS` conjuncts are not range bounds — each relates state \
         variables or pins one to a single value, which is a design claim, not the \
         space:\n  {}\n\
         Give each its own named invariant with a `Buggy` mutant that falsifies it, \
         or delete it if it only restates a guard or another invariant.",
        laundered.join("\n  ")
    );
}

/// The shape check is itself non-vacuous: a pin and a bound over the `Buggy`
/// dial are refused, while an ordinary two-sided bound passes.
#[test]
fn range_bound_check_refuses_pins_and_the_buggy_dial() {
    let var = || Box::new(Expr::Var("x"));
    let int = |n| Box::new(Expr::Int(n));
    let konst = |name| Box::new(Expr::ConstRef(name));
    let consts = [("Cap", 3), ("Buggy", 0)];
    let rejected = |e: Expr| !not_range_bounds(&e, &consts).is_empty();

    assert!(rejected(Expr::Le(var(), int(0))), "x <= 0 pins x");
    assert!(rejected(Expr::Gt(int(1), var())), "1 > x pins x");
    assert!(
        rejected(Expr::Le(var(), konst("Buggy"))),
        "Buggy is the dial"
    );
    assert!(
        rejected(Expr::And(
            Box::new(Expr::Gt(var(), int(0))),
            Box::new(Expr::Le(var(), int(1))),
        )),
        "x > 0 && x <= 1 pins x to one"
    );
    assert!(
        rejected(Expr::Le(var(), Box::new(Expr::Var("y")))),
        "a relation"
    );

    assert!(!rejected(Expr::Le(var(), konst("Cap"))));
    assert!(!rejected(Expr::And(
        Box::new(Expr::Gt(var(), int(0))),
        Box::new(Expr::Le(var(), Box::new(Expr::Sub(konst("Cap"), int(1))))),
    )));
}
