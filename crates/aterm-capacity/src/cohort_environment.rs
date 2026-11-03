// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `CapacityCohort` (`aterm_models_capacity::capacity_cohort_model`) that no function
//! here performs. The ledger's own steps bind where the real code runs:
//!
//! - `PublishNamespaceFence` and `ProvePredecessorDrain` on `Namespace::prepare_private`, which
//!   [`crate::cohort_namespace::Namespace::create_private`] calls: it exchanges the regular-file
//!   fence into the old ledger's name, checks that the retained predecessor directory is empty,
//!   and only then renames `.ready.pending` to `.ready`. One call, made before any claim exists,
//!   takes both steps, so the traces step them after it returns and observe the namespace before
//!   the first claim. Every later read reruns `Namespace::validate`, which refuses unless both
//!   hold, so a trace observes `namespace_ready` only as 1 (a refusal fails the read) and
//!   `unknown_predecessor` by listing the retained directory, 1 only in the unknown-predecessor
//!   control.
//! - `DurableArm` on `Laid::arm_cohort`, and `BindActualChild` on `Laid::bind_cohort`.
//! - `CancelMatchingNoChild` on `Laid::cancel_cohort`, and on `Laid::finish_unused` for the
//!   receipt a failed `arm_cohort` kept, which `arm_cohort`'s retry, `finish_cohort` and
//!   `drop_cohort` reach.
//! - `FinishDrainedCohort` on `Laid::finish_cohort` (a live holder). With `RetireDescription`, in
//!   one tombstone, on `Laid::drop_cohort` (a live holder's Drop of a bound birth whose group the
//!   kernel reports drained, never finished) and on the reaping scan's `retire_drained` (a
//!   drained claim no description holds, which every other reader keeps whole until then).
//! - `RetireDescription` alone on `Laid::drop_cohort` for a description, and on `retire_drained`
//!   for a free one.
//! - `AdmitSecond` on [`crate::Waiter::poll`]: its reaping look finds room beside every claim
//!   ahead under the namespace's validation, and its grant then writes the durable grant line
//!   through `Laid::admit_cohort` (the traced claim's own grant is the model's initial state).
//!
//! The conformance traces (`cohort_failure_tests.rs`) project each: the host-then-worker SIGKILL
//! trace, the live holder's drain with its finish and Drop or its Drop alone, the no-child
//! cancellation, which keeps the next request waiting until its Drop retires it, and the unknown
//! predecessor. No caller outside this crate reaches the cohort yet: managed frontend adoption
//! remains (`docs/DESIGN-capacity-cohort-custody-2026-11-02.md`). Test and debug-assertion
//! builds only.

/// The world's steps (the kernel's, another process's, another host's, a reboot's and a torn
/// write's), the caller's forced request, and each `Buggy` member of `CapacityCohort`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "LoseNamespaceReadiness",
        reason = "The namespace's fence, header or readiness file removed or replaced: another process's step on its files. Namespace::validate rechecks every recorded identity and the matching headers, so cohort_aware, every claim transaction, and with them ARM and admission, refuse until it holds again."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ObserveUnknownPredecessor",
        reason = "An entry appearing in the retained predecessor directory: an old v1 reader that held that directory's descriptor across the exchange writing into it, the world's step. Namespace::validate refuses while the directory holds any entry, so no claim transaction, ARM or admission proceeds."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "RequestForce",
        reason = "A caller asking to be admitted now (Admit::Now, `--admit now`): the caller's step. On a cohort-aware ledger Waiter::force only polls, so a forced request meets the same namespace, predecessor and room checks as any other."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ActualSpawn",
        reason = "The actual child's birth: the kernel's fork and exec through aterm-uds's spawnfd::Command::spawn_cohort, which consumes a one-use BirthAttempt in a new self-led group; no function here starts a process. The conformance traces step it at the actual spawn and project the child's group from the kernel."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ActualNoChild",
        reason = "This attempt's genuine no-child outcome: aterm-uds's NotSpawned receipt, from a spawn_cohort that failed before executing its target or from BirthAttempt::cancel, which only the original process can call; Laid::arm_cohort calls it itself when its ARM event fails with a write pending, and keeps the receipt as unused for Laid::finish_unused. The no-child conformance trace steps it at the failed spawn."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ForeignNoChild",
        reason = "A NotSpawned receipt of another attempt or process: made outside this claim. Laid::cancel_cohort refuses it, because Journal::cancel matches the receipt's key and owner to the durable ARM; the no-child conformance trace presents one."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "DropAllGuards",
        reason = "Every guard description closed: the holder, its workers and their children closing descriptors or exiting, the kernel's step. Nothing here releases on it: a reaping scan retires a free-lock claim only through retire_drained once its journal reads Drained, and every other read keeps an admitted free-lock claim's whole budget (Accounting::Whole) until that tombstone."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "HolderDies",
        reason = "The original holder process dying (the double-death trace's host SIGKILL): the kernel's step. Journal::decode then reads the holder as not current and gives its record Accounting::Whole."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "GroupLeaderDies",
        reason = "The bound group's leader exiting while a member remains (the double-death trace's worker SIGKILL beside its compiler): the kernel's step. Cohort::lifetime asks the kernel for the group (group_presence), never its leader, so the group stays Present."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "GroupActuallyDrains",
        reason = "The bound group's last member exiting: the kernel's step. Within the captured host, boot and pid namespace, Cohort::lifetime reads Drained only when aterm_uds::process::group_presence finds the group absent."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ScopeBecomesForeign",
        reason = "Another OS host reading a shared or copied state directory: the world's step. Scope::ended_or_unknown and Cohort::lifetime read another host as Unknown, and Namespace::validate refuses a namespace another host made."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ScopeBecomesUnreadable",
        reason = "The host, boot or pid-namespace read failing (Scope::read gives none): the kernel's step. The journal's and the cohort's lifetimes then read Unknown, which keeps the whole budget."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "SameHostReboots",
        reason = "A reboot of the same OS host: the world's step. Scope::ended_or_unknown and Cohort::lifetime read the same host's changed boot as Drained, after which a reaping scan's retire_drained may retire the claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "TailBecomesUnreadable",
        reason = "A torn or unrecognized journal tail: a crash's or the disk's step. Journal::decode never skips one (it needs the final newline and refuses an unknown line), and the checkpoint refuses a working file whose bytes or physical stamp differ from its last committed generation, so the scan fails and admission waits."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ReapFreeGuards",
        reason = "Buggy=1 negative control only; a free guard lock releasing an armed or bound budget, where a reaping scan retires a free-lock claim only through retire_drained once its journal reads Drained and otherwise keeps its whole budget (the double-death trace checks this mutant)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ReleaseDeadLeader",
        reason = "Buggy=2 negative control only; a dead leader releasing a group whose member lives, where Cohort::lifetime asks the kernel for the group (group_presence), never its leader"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "ForeignBootMeansDrained",
        reason = "Buggy=3 negative control only; another host's boot read as drain, where Scope::ended_or_unknown and Cohort::lifetime compare the host before the boot and read another host as Unknown"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "IgnoreTornTail",
        reason = "Buggy=4 negative control only; a torn tail read as a releasable description, where the checkpoint's generation check refuses a working file whose bytes or stamp differ from the last committed generation (and Journal::decode refuses an incomplete or unknown line in any capture), so the scan fails"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "CreditNumericMemoryAndAdmit",
        reason = "Buggy=5 negative control only; numeric process memory credited against a whole reservation, where Claim::credit refuses pid credit on a cohort claim and an Accounting::Whole record reserves its whole declared memory, crediting no holder or credited use"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "OldReaderAdmitsBeforeFence",
        reason = "Buggy=6 negative control only; a forced admission before the fence holds or beside an unknown predecessor, where Waiter::force on a cohort-aware ledger only polls and every claim transaction runs Namespace::validate, which needs the fence, readiness and an empty predecessor directory"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "CancelWithoutMatchingOutcome",
        reason = "Buggy=7 negative control only; an ARM cancelled without this attempt's own NotSpawned, where Laid::cancel_cohort goes through Journal::cancel, which matches the receipt's key and owner to the durable ARM (the no-child trace checks this mutant)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CapacityCohort",
        action = "SpawnBeforeDurableArm",
        reason = "Buggy=8 negative control only; a child started before its ARM is durable, where Laid::arm_cohort returns its BirthAttempt only after the ARM event commits and Laid::bind_cohort binds only a spawn whose key the journal durably armed"
    )
)]
#[doc(hidden)]
pub fn capacity_cohort_environment() {}
