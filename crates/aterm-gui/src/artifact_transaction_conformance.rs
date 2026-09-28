// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for fixed-path snapshot generations and handle-anchored
//! artifacts.
//!
//! The tests drive the shipping generation fence, `PinnedDir` accessors, and the
//! video lease registry, project their observable states onto the derived
//! models, and validate every concrete transition. Deliberately stale/outside
//! post-states are rejected as non-vacuous controls. `ArtifactReplyPublication`
//! is driven where its writer and ACK wait live, in `control`'s tests
//! (`capture_reply_*`); its projection is here.

#![cfg(test)]

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_render::Frame;
use aterm_spec::derive::{
    Model, anchored_artifact_transaction_model, artifact_reader_lease_model,
    snapshot_generation_commit_model, video_batch_publication_durability_model,
};
use aterm_spec::interp::State;
use aterm_spec::verify::validate_transition_tiered;

use crate::app_introspect::{SnapshotPng, begin_snapshot_generation, write_snapshot_artifacts};
use crate::control_auth::ConfinedImage;
use crate::pinned_dir::PinnedDir;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AnchoredObservation {
    phase: i64,
    swapped: bool,
    path_identity: i64,
    operation: i64,
    effect_target: i64,
    reply: i64,
    certified_identity: i64,
}

pub(crate) fn project_anchored(model: &Model, observed: AnchoredObservation) -> State {
    let mut state = model.init_state();
    state.insert("phase", observed.phase);
    state.insert("swapped", i64::from(observed.swapped));
    state.insert("path_identity", observed.path_identity);
    state.insert("operation", observed.operation);
    state.insert("effect_target", observed.effect_target);
    state.insert("reply", observed.reply);
    state.insert("certified_identity", observed.certified_identity);
    state
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapshotObservation {
    latest: i64,
    job: i64,
    payload: i64,
    done: bool,
}

pub(crate) fn project_snapshot(model: &Model, observed: SnapshotObservation) -> State {
    let mut state = model.init_state();
    state.insert("latest", observed.latest);
    state.insert("job", observed.job);
    state.insert("payload", observed.payload);
    state.insert("done", i64::from(observed.done));
    state
}

/// Test-visible projection of one guarded artifact reply. As `control`'s
/// `capture_reply_*` binds read the shipping capture reply: `artifact` is the
/// advertised file, `guard` the name lease only the capture guard holds once
/// the file is written, `committed`/`reply`/`challenge` the bytes the writer
/// produced (the OK body; the complete body-then-trailer frame; any nonce
/// trailer), and `ack`/`ack_failed`/`write_error` the writer's and ACK wait's
/// verdicts. `phase` and `quarantine_age` are drive coordinates.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactReplyObservation {
    pub(crate) phase: i64,
    pub(crate) artifact: bool,
    pub(crate) guard: bool,
    pub(crate) committed: bool,
    pub(crate) reply: bool,
    pub(crate) challenge: bool,
    pub(crate) ack: bool,
    pub(crate) ack_failed: bool,
    pub(crate) write_error: bool,
    pub(crate) quarantine_age: i64,
}

pub(crate) fn project_artifact_reply(model: &Model, observed: ArtifactReplyObservation) -> State {
    let mut state = model.init_state();
    state.insert("phase", observed.phase);
    state.insert("artifact", i64::from(observed.artifact));
    state.insert("guard", i64::from(observed.guard));
    state.insert("committed", i64::from(observed.committed));
    state.insert("reply", i64::from(observed.reply));
    state.insert("challenge", i64::from(observed.challenge));
    state.insert("ack", i64::from(observed.ack));
    state.insert("ack_failed", i64::from(observed.ack_failed));
    state.insert("write_error", i64::from(observed.write_error));
    state.insert("quarantine_age", observed.quarantine_age);
    state
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactHandoffCapacityObservation {
    pub(crate) live: i64,
    pub(crate) descriptor_units: i64,
    pub(crate) charge_one: i64,
    pub(crate) charge_two: i64,
    pub(crate) charge_three: i64,
    pub(crate) selected: i64,
}

pub(crate) fn project_artifact_handoff_capacity(
    model: &Model,
    observed: ArtifactHandoffCapacityObservation,
) -> State {
    let mut state = model.init_state();
    state.insert("live", observed.live);
    state.insert("descriptor_units", observed.descriptor_units);
    state.insert("charge_one", observed.charge_one);
    state.insert("charge_two", observed.charge_two);
    state.insert("charge_three", observed.charge_three);
    state.insert("selected", observed.selected);
    state
}

/// Test-visible projection of the durable video-batch publication boundary.
/// A counted member has completed its per-file sync; `synced_members` is the
/// prefix covered by the most recent recording-directory barrier.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VideoBatchPublicationObservation {
    pub(crate) members: i64,
    pub(crate) synced_members: i64,
    pub(crate) marker: bool,
}

pub(crate) fn project_video_batch_publication_durability(
    model: &Model,
    observed: VideoBatchPublicationObservation,
) -> State {
    let mut state = model.init_state();
    state.insert("members", observed.members);
    state.insert("synced_members", observed.synced_members);
    state.insert("marker", i64::from(observed.marker));
    state
}

/// Test-visible projection of the refcounted recording-lease registry.
///
/// Production anchors intentionally name this exact function. Their concrete
/// observations are: registry `count` -> `leases`, the entry's
/// `video_sweep_requested` -> `armed`, an entry whose reserved `video_retention`
/// admission is gone -> `admission_spent`, retained recording-identity
/// disagreement -> `identity_mismatch`, the last-release handoff -> `pending`,
/// and callback execution -> `sweeping`. `requested` is the caller's history —
/// an arm the test made and no finished sweep has discharged — so a registry
/// that lost an armed entry still reads as owing its sweep.
/// `replacement_joined` is a negative-control witness and is always false for
/// shipping code.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactReaderObservation {
    leases: i64,
    armed: bool,
    requested: bool,
    pending: bool,
    sweeping: bool,
    admission_spent: bool,
    identity_mismatch: bool,
    replacement_joined: bool,
}

pub(crate) fn project_artifact_reader_lease(
    model: &Model,
    observed: ArtifactReaderObservation,
) -> State {
    let mut state = model.init_state();
    state.insert("leases", observed.leases);
    state.insert("armed", i64::from(observed.armed));
    state.insert("requested", i64::from(observed.requested));
    state.insert("pending", i64::from(observed.pending));
    state.insert("sweeping", i64::from(observed.sweeping));
    state.insert("admission_spent", i64::from(observed.admission_spent));
    state.insert("identity_mismatch", i64::from(observed.identity_mismatch));
    state.insert("replacement_joined", i64::from(observed.replacement_joined));
    state
}

fn unconfined() -> AnchoredObservation {
    AnchoredObservation {
        phase: 0,
        swapped: false,
        path_identity: 0,
        operation: 0,
        effect_target: 0,
        reply: 0,
        certified_identity: 0,
    }
}

fn pinned() -> AnchoredObservation {
    AnchoredObservation {
        phase: 1,
        swapped: false,
        path_identity: 1,
        ..unconfined()
    }
}

fn operated(operation: i64) -> AnchoredObservation {
    AnchoredObservation {
        phase: 2,
        operation,
        effect_target: 1,
        ..pinned()
    }
}

fn replied(operation: i64) -> AnchoredObservation {
    AnchoredObservation {
        phase: 3,
        operation,
        effect_target: 1,
        reply: 1,
        certified_identity: 1,
        ..pinned()
    }
}

pub(crate) fn assert_transition(
    model: &Model,
    action: &str,
    before: &State,
    after: &State,
    label: &str,
) {
    assert!(
        model.action_enabled(action, before),
        "{label}: {action} is disabled for {before:?}"
    );
    assert!(
        model.successors(action, before).contains(after),
        "{label}: {action} does not admit {before:?} -> {after:?}"
    );
    let (conforms, evidence) =
        validate_transition_tiered(model, &[], before, after, Some(action), label);
    assert!(conforms, "{label}: {evidence}");
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "{label}: {} fails in {after:?}",
            invariant.name
        );
    }
}

pub(crate) fn reject_transition(
    model: &Model,
    action: &str,
    before: &State,
    after: &State,
    label: &str,
) {
    assert!(
        !model.successors(action, before).contains(after),
        "{label}: mutant unexpectedly appears in {action} successors"
    );
    let (conforms, evidence) =
        validate_transition_tiered(model, &[], before, after, Some(action), label);
    assert!(
        !conforms,
        "{label}: mutant unexpectedly conformed: {evidence}"
    );
}

fn unique_dir(label: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "aterm-artifact-conformance-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn marker_generation(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .expect("read completion marker")
        .lines()
        .find_map(|line| line.strip_prefix("generation="))
        .expect("generation field")
        .parse()
        .expect("numeric generation")
}

#[aterm_spec::spec_unmodeled(
    machine = "SnapshotGenerationCommit",
    action = "SelectCurrent",
    reason = "environment scheduling: selecting the newest worker changes which queued job the \
              bounded model drives; begin/old-commit/current-commit are bound to shipping code"
)]
#[aterm_spec::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "SwapAncestor",
    reason = "adversarial filesystem environment action: another same-uid process replaces a \
              pathname ancestor; the Unix Tier-1 test drives the observation explicitly"
)]
#[aterm_spec::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyReresolveRead",
    reason = "Buggy=1 negative-control action only; shipping reads remain relative to the retained \
              directory handle and the Tier-1 outside-target mutant is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyReresolveWrite",
    reason = "Buggy=1 negative-control action only; shipping writes remain relative to the \
              retained directory handle and never implement this transition"
)]
#[aterm_spec::spec_unmodeled(
    machine = "AnchoredArtifactTransaction",
    action = "BuggyCertifySwapped",
    reason = "Buggy=1 negative-control action only; reply validation fails closed after identity \
              drift and the Tier-1 false-success mutant is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyPublishAfterCancel",
    reason = "Buggy=1 negative control only; the real cancellation/authorization CAS has one \
              winner and the authorized write hook rejects a cancelled final-name publish"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyDropBeforeWrite",
    reason = "Buggy=1 negative control only; ControlReply owns ReplyRetention through write_all, \
              flush, and either valid ACK release or central-quarantine expiry"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyPruneLeased",
    reason = "Buggy=1 negative control only; retention holds the shared path-lease mutex across its \
              exact mutation and skips every still-leased image or recording"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyReleaseWithoutAck",
    reason = "Buggy=1 negative control only; only a matching nonce echo releases immediately, \
              while EOF/timeout/protocol failure transfers the guard to central quarantine"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyAcceptPreChallengeAck",
    reason = "Buggy=1 negative control only; aterm-ctl can echo only the fresh nonce trailer it \
              reads after the complete response, never a pre-pipelined acknowledgement"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyReleaseQuarantineEarly",
    reason = "Buggy=1 negative control only; failed or half-closed clients retain the publication guard \
              through the additional 30-second central-quarantine expiry"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyChallengeBeforeBody",
    reason = "Buggy=1 negative control only; the nonce trailer is written after the complete body \
              in write_control_reply_with_timeout_arm, and the Tier-1 early trailer is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyOkBeforeRevalidation",
    reason = "Buggy=1 negative control only; write_control_reply_with_timeout_arm revalidates the \
              guard before any OK byte, and the Tier-1 OK-then-ERR wire is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyTrailerErrorIgnored",
    reason = "Buggy=1 negative control only; a failed trailer write returns the error and \
              quarantines the guard, and the Tier-1 partial frame read as written is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReplyPublication",
    action = "BuggyAbortRetainsArtifact",
    reason = "Buggy=1 negative control only; an uncommitted reply guard removes its exact file on \
              drop, and the Tier-1 orphaned-file release is rejected"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyOverbook",
    reason = "Buggy=1 negative control only; the real locked admission update refuses when the \
              process-wide handoff count or descriptor-unit budget reaches its fixed limit"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyReconcileOverbook",
    reason = "Buggy=1 negative control only; exact-path reconciliation uses the same locked \
              aggregate descriptor budget and leaves the provisional charge intact on refusal"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyRefuseLeaksSlot",
    reason = "Buggy=1 negative control only; try_acquire_from decides admission before it charges \
              either counter, so a refusal leaves the pool untouched"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "BuggyReleaseProvisionalCharge",
    reason = "Buggy=1 negative control only; reconciliation records the permit's new charge, so \
              its drop returns exactly the units it holds"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectOne",
    reason = "ghost scheduler choice selecting a one-unit permit from the bounded charge multiset"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectTwo",
    reason = "ghost scheduler choice selecting a two-unit permit from the bounded charge multiset"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactHandoffCapacity",
    action = "SelectThree",
    reason = "ghost scheduler choice selecting a three-unit permit from the bounded charge multiset"
)]
#[aterm_spec::spec_unmodeled(
    machine = "VideoBatchPublicationDurability",
    action = "BuggyPublishBeforeSync",
    reason = "Buggy=1 negative control only; the real marker write is reachable only after \
              ConfinedVideoDir::publish completes the recording-directory batch barrier"
)]
#[aterm_spec::spec_unmodeled(
    machine = "VideoBatchPublicationDurability",
    action = "BuggySyncAhead",
    reason = "Buggy=1 negative control only; every member write clears batch_synced before it \
              lands, so a barrier never covers a later member \
              (video_member_write_after_sync_invalidates_marker_guard)"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyStartSweepEarly",
    reason = "Buggy=1 negative control only; the concrete registry starts its capability-bound \
              sweep only on the last refcount release"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyAcquireDuringSweep",
    reason = "Buggy=1 negative control only; retain_video_artifact_path fails closed while the \
              last-release callback owns the registry's sweeping state"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "ReplaceIdentity",
    reason = "external same-uid recording replacement is an environment transition; the concrete \
              registry compares the retained child identity before incrementing its lease count"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyArmedReleaseDropsEntry",
    reason = "Buggy=1 negative control only; the last release of an armed entry marks it \
              sweeping and hands its capability to the priority cleanup lane instead of \
              removing it"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyReleaseSweepsUnarmed",
    reason = "Buggy=1 negative control only; the last release starts a sweep only when \
              video_sweep_requested is set, and otherwise removes the registry entry"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyFinishKeepsEntry",
    reason = "Buggy=1 negative control only; finish_video_retention_sweep removes the finished \
              entry, taking its arm and recorded identity with it"
)]
#[aterm_spec::spec_unmodeled(
    machine = "ArtifactReaderLease",
    action = "BuggyAcquireReplacedIdentity",
    reason = "Buggy=1 negative control only; a mismatched retained recording identity is rejected \
              without joining the existing lexical-path lease group"
)]
#[expect(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
)]
fn explicit_environment_and_mutant_scope() {}

#[test]
fn artifact_xrefs_cover_every_action_with_named_projections_or_waivers() {
    let anchored: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "AnchoredArtifactTransaction")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a concrete projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        anchored,
        BTreeSet::from(["ConfinePin", "ReadPinned", "ValidateReply", "WritePinned"])
    );
    let anchored_waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "AnchoredArtifactTransaction")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        anchored_waivers,
        BTreeSet::from([
            "BuggyCertifySwapped",
            "BuggyReresolveRead",
            "BuggyReresolveWrite",
            "SwapAncestor",
        ])
    );

    let snapshot: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "SnapshotGenerationCommit")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a concrete projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        snapshot,
        BTreeSet::from(["BeginNew", "CommitCurrent", "CommitOld"])
    );
    let snapshot_waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "SnapshotGenerationCommit")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(snapshot_waivers, BTreeSet::from(["SelectCurrent"]));
}

#[test]
fn artifact_reply_and_reader_xrefs_cover_every_shipping_transition() {
    let refinements: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "ArtifactReplyPublication")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        refinements,
        BTreeSet::from([
            "AbortAuthorized",
            "AbortQueued",
            "AdvanceQuarantine",
            "AcknowledgeFailed",
            "AcknowledgePeer",
            "AuthorizeCommit",
            "Cancel",
            "ExpireQuarantine",
            "PrepareFailed",
            "PrepareWire",
            "QueueGuard",
            "ReleaseGuard",
            "RetentionSweep",
            "WriteFailed",
            "WriteWire",
        ])
    );
    let waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "ArtifactReplyPublication")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        waivers,
        BTreeSet::from([
            "BuggyAbortRetainsArtifact",
            "BuggyAcceptPreChallengeAck",
            "BuggyChallengeBeforeBody",
            "BuggyDropBeforeWrite",
            "BuggyOkBeforeRevalidation",
            "BuggyPruneLeased",
            "BuggyPublishAfterCancel",
            "BuggyReleaseQuarantineEarly",
            "BuggyReleaseWithoutAck",
            "BuggyTrailerErrorIgnored",
        ])
    );

    let handoff_refinements: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "ArtifactHandoffCapacity")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a concrete handoff-capacity projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        handoff_refinements,
        BTreeSet::from([
            "Acquire",
            "ReconcileGrow",
            "ReconcileShrink",
            "RefuseAtCap",
            "RefuseReconcile",
            "Release",
        ])
    );
    let handoff_waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "ArtifactHandoffCapacity")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        handoff_waivers,
        BTreeSet::from([
            "BuggyOverbook",
            "BuggyReconcileOverbook",
            "BuggyRefuseLeaksSlot",
            "BuggyReleaseProvisionalCharge",
            "SelectOne",
            "SelectThree",
            "SelectTwo",
        ])
    );

    let batch_refinements: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "VideoBatchPublicationDurability")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a concrete batch-durability projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        batch_refinements,
        BTreeSet::from(["PublishMarker", "SyncBatch", "WriteMember"])
    );
    let batch_waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "VideoBatchPublicationDurability")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        batch_waivers,
        BTreeSet::from(["BuggyPublishBeforeSync", "BuggySyncAhead"])
    );

    let reader_refinements: BTreeSet<_> = aterm_spec::xref::refinements()
        .filter(|anchor| anchor.machine == "ArtifactReaderLease")
        .map(|anchor| {
            assert!(
                !anchor.project.is_empty(),
                "{} needs a concrete reader-registry projection",
                anchor.action
            );
            anchor.action
        })
        .collect();
    assert_eq!(
        reader_refinements,
        BTreeSet::from([
            "Acquire",
            "Arm",
            "FinishSweep",
            "RejectAcquireWhileSweeping",
            "RejectReplacedIdentity",
            "Release",
            "StartSweep",
        ])
    );
    let reader_waivers: BTreeSet<_> = aterm_spec::xref::waivers()
        .filter(|waiver| waiver.machine == "ArtifactReaderLease")
        .map(|waiver| waiver.action)
        .collect();
    assert_eq!(
        reader_waivers,
        BTreeSet::from([
            "BuggyAcquireDuringSweep",
            "BuggyAcquireReplacedIdentity",
            "BuggyArmedReleaseDropsEntry",
            "BuggyFinishKeepsEntry",
            "BuggyReleaseSweepsUnarmed",
            "BuggyStartSweepEarly",
            "ReplaceIdentity",
        ])
    );
}

#[test]
fn real_video_batch_publication_conforms_to_directory_barrier_ordering() {
    let root = unique_dir("video-batch-durability");
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("create private control root");
    let mut recording =
        crate::control_auth::confine_video_dir(&root).expect("confine fresh video recording");
    let recording_path = recording.path().to_path_buf();
    let model = video_batch_publication_durability_model();
    let initial = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation::default(),
    );
    assert!(!recording.batch_synced_for_test());

    let frame = recording
        .write_sealed_frame(OsStr::new("frame_0001.png"), b"png")
        .expect("file-sync frame member");
    assert!(
        !recording.batch_synced_for_test(),
        "a member write leaves the directory batch unsynced"
    );
    let one_member = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 1,
            ..VideoBatchPublicationObservation::default()
        },
    );
    assert_transition(
        &model,
        "WriteMember",
        &initial,
        &one_member,
        "sealed frame completes its per-file durability boundary",
    );

    let index = recording
        .write_batch_member_authorized(
            OsStr::new("index.json"),
            br#"{"frames":[{"file":"frame_0001.png"}]}"#,
            || true,
        )
        .expect("file-sync index member");
    assert!(
        !recording.batch_synced_for_test(),
        "the complete member set still needs its directory barrier"
    );
    let complete_members = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 2,
            ..VideoBatchPublicationObservation::default()
        },
    );
    assert_transition(
        &model,
        "WriteMember",
        &one_member,
        &complete_members,
        "index completes the second per-file durability boundary",
    );

    let premature_marker = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 2,
            marker: true,
            ..VideoBatchPublicationObservation::default()
        },
    );
    reject_transition(
        &model,
        "PublishMarker",
        &complete_members,
        &premature_marker,
        "publication marker cannot precede the directory batch barrier",
    );
    assert!(!model.check_invariant("MarkerCoversEveryMember", &premature_marker));
    recording
        .publish_marker()
        .expect_err("shipping marker guard rejects a batch with no directory barrier");
    assert!(!recording.batch_synced_for_test());
    assert!(
        !recording_path
            .join(crate::control_auth::VIDEO_PUBLISHED_FILE)
            .exists(),
        "shipping writes leave the reader-visible marker absent before the barrier"
    );

    let published_path = recording
        .publish(std::slice::from_ref(&frame), &index)
        .expect("sync complete batch directory");
    assert_eq!(published_path, recording_path);
    assert!(
        recording.batch_synced_for_test(),
        "successful publish records the concrete directory barrier"
    );
    let batch_synced = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 2,
            synced_members: 2,
            marker: false,
        },
    );
    assert_transition(
        &model,
        "SyncBatch",
        &complete_members,
        &batch_synced,
        "recording publish completes the directory batch barrier",
    );

    let marker_guard = recording
        .publish_marker()
        .expect("publish reader-visible marker after barrier");
    let marker_visible = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 2,
            synced_members: 2,
            marker: true,
        },
    );
    assert_transition(
        &model,
        "PublishMarker",
        &batch_synced,
        &marker_visible,
        "marker publication follows the complete batch barrier",
    );
    assert!(
        recording_path
            .join(crate::control_auth::VIDEO_PUBLISHED_FILE)
            .is_file()
    );

    drop(marker_guard);
    drop(index);
    drop(recording);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn published_video_batch_rejects_every_late_mutation() {
    let root = unique_dir("video-batch-immutable");
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("create private control root");
    let mut recording =
        crate::control_auth::confine_video_dir(&root).expect("confine fresh video recording");
    let recording_path = recording.path().to_path_buf();
    let index_path = recording_path.join("index.json");
    let marker_path = recording_path.join(crate::control_auth::VIDEO_PUBLISHED_FILE);
    let model = video_batch_publication_durability_model();
    let initial = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation::default(),
    );

    let index = recording
        .write_batch_member_authorized(OsStr::new("index.json"), br#"{"frames":[]}"#, || true)
        .expect("write the sole batch member");
    let one_member = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 1,
            ..VideoBatchPublicationObservation::default()
        },
    );
    assert_transition(
        &model,
        "WriteMember",
        &initial,
        &one_member,
        "index completes its per-file durability boundary",
    );

    recording
        .publish(&[], &index)
        .expect("sync batch directory");
    let batch_synced = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 1,
            synced_members: 1,
            marker: false,
        },
    );
    assert_transition(
        &model,
        "SyncBatch",
        &one_member,
        &batch_synced,
        "publish completes the directory batch barrier",
    );

    let marker_guard = recording
        .publish_marker()
        .expect("publish marker after the batch barrier");
    let marker_visible = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 1,
            synced_members: 1,
            marker: true,
        },
    );
    assert_transition(
        &model,
        "PublishMarker",
        &batch_synced,
        &marker_visible,
        "marker publication makes the batch immutable",
    );

    let index_before = std::fs::read(&index_path).expect("read published index");
    let marker_before = std::fs::read(&marker_path).expect("read publication marker");
    let late_member = project_video_batch_publication_durability(
        &model,
        VideoBatchPublicationObservation {
            members: 2,
            synced_members: 1,
            marker: true,
        },
    );
    assert!(
        model.action_enabled("WriteMember", &batch_synced),
        "negative control: capacity remains for a second member before the marker"
    );
    reject_transition(
        &model,
        "WriteMember",
        &marker_visible,
        &late_member,
        "a marker-visible batch rejects a later member",
    );
    reject_transition(
        &model,
        "SyncBatch",
        &marker_visible,
        &marker_visible,
        "a marker-visible batch rejects a repeated barrier",
    );
    reject_transition(
        &model,
        "PublishMarker",
        &marker_visible,
        &marker_visible,
        "a marker-visible batch rejects a repeated marker",
    );

    let authorized = std::cell::Cell::new(false);
    let late_batch =
        recording.write_batch_member_authorized(OsStr::new("late-index.json"), b"late", || {
            authorized.set(true);
            true
        });
    assert_eq!(
        late_batch
            .expect_err("published batch rejects an authorized member")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(
        !authorized.get(),
        "immutability rejects before invoking commit authorization"
    );
    assert_eq!(
        recording
            .write_sealed_frame(OsStr::new("late-frame.png"), b"late")
            .expect_err("published batch rejects a sealed frame")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        recording
            .write_new_private(OsStr::new("late-test-member"), b"late")
            .expect_err("published batch rejects the test-only member writer")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    for name in ["index.json", crate::control_auth::VIDEO_PUBLISHED_FILE] {
        assert_eq!(
            recording
                .remove_file_if_exists(OsStr::new(name))
                .expect_err("published batch rejects member removal")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        recording
            .publish(&[], &index)
            .expect_err("published batch rejects a repeated barrier")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        recording
            .publish_marker()
            .expect_err("published batch rejects a repeated marker")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(
        recording.batch_synced_for_test(),
        "rejected mutations leave the published projection unchanged"
    );
    assert_eq!(
        recording
            .abort()
            .expect_err("published batch is permanently non-abortable")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );

    assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
    assert_eq!(std::fs::read(&marker_path).unwrap(), marker_before);
    assert!(!recording_path.join("late-index.json").exists());
    assert!(!recording_path.join("late-frame.png").exists());
    assert!(!recording_path.join("late-test-member").exists());
    assert!(recording_path.is_dir());

    drop(marker_guard);
    drop(index);
    let _ = std::fs::remove_dir_all(root);
}

fn write_published_recording(root: &Path, name: &str) {
    let recording = root.join(name);
    std::fs::create_dir(&recording).unwrap();
    std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
    std::fs::write(
        recording.join(crate::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();
}

fn published_recording_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry.path().join("index.json").is_file()
                && entry
                    .path()
                    .join(crate::control_auth::VIDEO_PUBLISHED_FILE)
                    .is_file()
        })
        .count()
}

/// A private recordings root holding `count` published recordings. Returns the
/// fixture directory, the root, the newest recording's name, and the registry
/// key `retain_video_artifact_path` files that recording under.
fn reader_fixture(label: &str, count: usize) -> (PathBuf, PathBuf, std::ffi::OsString, PathBuf) {
    let dir = unique_dir(label);
    let _ = std::fs::remove_dir_all(&dir);
    crate::control_auth::ensure_private_dir(&dir).unwrap();
    let root = dir.join("recordings");
    crate::control_auth::ensure_private_dir(&root).unwrap();
    for sequence in 0..count {
        write_published_recording(&root, &format!("rec-{sequence:020}-000"));
    }
    let fresh = std::ffi::OsString::from(format!("rec-{:020}-000", count - 1));
    let key = PinnedDir::open_resolved(&root).unwrap().path().join(&fresh);
    (dir, root, fresh, key)
}

/// One shipping `video frames` reader lease on `fresh`, with the recording
/// handle its arm validates against. `None` is the registry refusing a lease
/// while a sweep owns the entry.
fn reader_lease(
    root: &Path,
    fresh: &OsStr,
) -> Option<(crate::control_auth::ArtifactPathLease, PinnedDir)> {
    let pinned_root = PinnedDir::open_resolved(root).unwrap();
    let recording = pinned_root.child(fresh).unwrap();
    crate::control_auth::retain_video_artifact_path(pinned_root, fresh.to_os_string(), &recording)
        .unwrap()
        .map(|lease| (lease, recording))
}

/// Read the shipping lease registry. `leases`, `armed` and `sweeping` are the
/// entry's `count`, `video_sweep_requested` and `sweeping`, and
/// `admission_spent` is an entry without its `video_retention`; an absent entry
/// is idle. `pending` is never observable — the last decrement and the sweep's
/// start happen under one registry mutex — and `requested`/`identity_mismatch`
/// are the history this test drove.
fn observe_reader(model: &Model, key: &Path, requested: bool, identity_mismatch: bool) -> State {
    let entry = crate::control_auth::artifact_lease_registry_for_test(key);
    project_artifact_reader_lease(
        model,
        ArtifactReaderObservation {
            leases: i64::try_from(entry.map_or(0, |entry| entry.count)).unwrap(),
            armed: entry.is_some_and(|entry| entry.sweep_requested),
            requested,
            pending: false,
            sweeping: entry.is_some_and(|entry| entry.sweeping),
            admission_spent: entry.is_some_and(|entry| !entry.admission_held),
            identity_mismatch,
            replacement_joined: false,
        },
    )
}

/// TIER-1: the shipping lease registry — `retain_video_artifact_path`,
/// `arm_video_retention_sweep`, a lease's drop, and the cleanup worker's
/// sweep — read back after every step. Twelve published recordings over the
/// keep-limit of eight make the sweep's work visible: a sweep that ran prunes.
#[cfg(unix)]
#[test]
fn real_video_reader_registry_conforms_to_last_release_sweep_lifecycle() {
    let model = artifact_reader_lease_model();
    let (dir, root, fresh, key) = reader_fixture("reader-sweep", 12);
    let idle = observe_reader(&model, &key, false, false);
    assert_eq!(idle, model.init_state());

    let (first, first_recording) = reader_lease(&root, &fresh).expect("first reader lease");
    let reader_one = observe_reader(&model, &key, false, false);
    assert_transition(&model, "Acquire", &idle, &reader_one, "first reader lease");
    let (second, second_recording) = reader_lease(&root, &fresh).expect("second reader lease");
    let reader_two = observe_reader(&model, &key, false, false);
    assert_transition(
        &model,
        "Acquire",
        &reader_one,
        &reader_two,
        "shared reader lease",
    );

    first.arm_video_retention_sweep(&first_recording).unwrap();
    let armed_two = observe_reader(&model, &key, true, false);
    assert_transition(
        &model,
        "Arm",
        &reader_two,
        &armed_two,
        "final reader identity validation requests the sweep",
    );
    drop(first_recording);
    drop(second_recording);

    drop(first);
    let armed_one = observe_reader(&model, &key, true, false);
    assert_transition(
        &model,
        "Release",
        &armed_two,
        &armed_one,
        "a non-final release keeps the lease group",
    );
    assert_eq!(published_recording_count(&root), 12, "nothing swept yet");
    let early_sweep = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            armed: true,
            requested: true,
            sweeping: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "StartSweep",
        &armed_one,
        &early_sweep,
        "a non-final release cannot start convergence",
    );

    // The final drop decrements and starts the sweep under one mutex, and the
    // cleanup worker finishes it. Park the worker first, so the sweeping entry
    // stays open and the refusal below is judged on every run, not only when
    // this thread happens to beat the worker.
    let pending = model.successors("Release", &armed_one)[0].clone();
    let sweeping = model.successors("StartSweep", &pending)[0].clone();
    // The seam between the last decrement and the sweep's start is crossed
    // under that one mutex, so real code never shows it; the model must still
    // refuse a lease entering it.
    let entered_seam = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            armed: true,
            requested: true,
            pending: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "Acquire",
        &pending,
        &entered_seam,
        "no producer-or-reader lease may enter the last-release/sweep interval",
    );
    // The final release that removes the armed entry instead of sweeping it —
    // the `video_sweep_requested` case folded into the plain removal, or the
    // release-build `_ =>` arm. The registry then reads idle, but the arm this
    // test made is still owed its sweep.
    let dropped_entry = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            requested: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "Release",
        &armed_one,
        &dropped_entry,
        "the last armed release schedules its sweep instead of forgetting it",
    );
    assert!(!model.check_invariant("RequestedRetentionRunsAtLastRelease", &dropped_entry));
    let hold = crate::control_auth::hold_artifact_cleanup_for_test();
    drop(second);
    assert_eq!(
        crate::control_auth::artifact_lease_registry_for_test(&key),
        Some(crate::control_auth::ArtifactLeaseEntryForTest {
            count: 0,
            sweep_requested: true,
            sweeping: true,
            admission_held: false,
        }),
        "the final drop left the entry sweeping, its admission handed to the sweep"
    );
    assert_eq!(observe_reader(&model, &key, true, false), sweeping);
    assert!(
        reader_lease(&root, &fresh).is_none(),
        "acquisition fails closed while the sweep owns the entry"
    );
    assert_transition(
        &model,
        "RejectAcquireWhileSweeping",
        &sweeping,
        &observe_reader(&model, &key, true, false),
        "acquisition fails closed while the sweep owns the entry",
    );
    drop(hold);
    // Waits until the entry is GONE: a finish that reset the entry's flags and
    // kept it (with its recorded identity) times out here.
    crate::control_auth::wait_for_artifact_cleanup_for_test(&key);
    let swept = observe_reader(&model, &key, false, false);
    assert_transition(
        &model,
        "FinishSweep",
        &sweeping,
        &swept,
        "completion removes the entry and reopens acquisition",
    );
    // A finish that reset the entry's flags and kept it: the entry has spent
    // its admission, so `join_video_artifact_state` would refuse every reader.
    let kept_entry = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            admission_spent: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "FinishSweep",
        &sweeping,
        &kept_entry,
        "a finished sweep removes its entry",
    );
    assert!(!model.check_invariant("IdleNameAdmitsReaders", &kept_entry));
    let retained = published_recording_count(&root);
    assert!(
        retained < 12,
        "the last armed release ran the retention sweep ({retained} recordings left)"
    );

    // Re-seed four recordings older than every survivor, so a sweep the next
    // release had no business running would visibly prune them.
    for sequence in 0..4 {
        write_published_recording(&root, &format!("rec-{sequence:020}-000"));
    }
    let reseeded = published_recording_count(&root);
    assert_eq!(reseeded, retained + 4);
    let (again, again_recording) = reader_lease(&root, &fresh).expect("reopened lease");
    let reacquired = observe_reader(&model, &key, false, false);
    assert_transition(
        &model,
        "Acquire",
        &swept,
        &reacquired,
        "acquisition after completed convergence",
    );
    drop(again_recording);
    drop(again);
    let released = observe_reader(&model, &key, false, false);
    assert_transition(
        &model,
        "Release",
        &reacquired,
        &released,
        "an unarmed final release removes the entry",
    );
    crate::control_auth::wait_for_artifact_cleanup_for_test(&key);
    assert_eq!(
        published_recording_count(&root),
        reseeded,
        "an unarmed final release sweeps nothing"
    );
    // The same release scheduling a sweep nothing armed: it could never start,
    // and the path would refuse every later reader.
    let mut stranded = released.clone();
    stranded.insert("pending", 1);
    reject_transition(
        &model,
        "Release",
        &reacquired,
        &stranded,
        "an unarmed final release schedules no sweep",
    );
    assert!(!model.check_invariant("MaintenanceRequiresArm", &stranded));

    let _ = std::fs::remove_dir_all(dir);
}

/// TIER-1: a same-name replacement cannot join a live recording's lease group.
/// The replacement is the environment's move; the refusal is the registry's.
#[cfg(unix)]
#[test]
fn real_video_reader_registry_refuses_a_replaced_recording() {
    let model = artifact_reader_lease_model();
    let (dir, root, fresh, key) = reader_fixture("reader-identity", 1);
    let (first, first_recording) = reader_lease(&root, &fresh).expect("first reader lease");
    drop(first_recording);
    let reader_one = observe_reader(&model, &key, false, false);

    std::fs::rename(root.join(&fresh), root.join("recording-original")).unwrap();
    write_published_recording(&root, fresh.to_str().unwrap());
    let replaced_one = observe_reader(&model, &key, false, true);
    assert_transition(
        &model,
        "ReplaceIdentity",
        &reader_one,
        &replaced_one,
        "a same-uid process replaces the recording at its name",
    );

    let pinned_root = PinnedDir::open_resolved(&root).unwrap();
    let replacement = pinned_root.child(&fresh).unwrap();
    let error =
        crate::control_auth::retain_video_artifact_path(pinned_root, fresh.clone(), &replacement)
            .expect_err("a replacement cannot join the live lease group");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    let refused = observe_reader(&model, &key, false, true);
    assert_transition(
        &model,
        "RejectReplacedIdentity",
        &replaced_one,
        &refused,
        "the registry refuses the replaced identity",
    );
    let joined = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 2,
            identity_mismatch: true,
            replacement_joined: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "Acquire",
        &replaced_one,
        &joined,
        "ordinary acquisition cannot admit a replaced recording identity",
    );

    drop(replacement);
    drop(first);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn real_snapshot_generation_fence_conforms_and_rejects_stale_commit_mutant() {
    let root = unique_dir("snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("snapshot.png");
    let text_path = sidecar(&path, ".txt");
    let done_path = sidecar(&path, ".done");
    let frame_one = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0011_2233],
    };
    let frame_two = Frame {
        width: 1,
        height: 1,
        pixels: vec![0x0044_5566],
    };
    let model = snapshot_generation_commit_model();

    let first = begin_snapshot_generation(&path).expect("begin generation one");
    assert_eq!(first.generation(), 1);
    let initial = project_snapshot(
        &model,
        SnapshotObservation {
            latest: 1,
            job: 1,
            payload: 0,
            done: false,
        },
    );
    assert_eq!(initial, model.init_state());

    write_snapshot_artifacts(&SnapshotPng::encode(&frame_one), "generation-one", &first)
        .expect("commit generation one");
    assert_eq!(marker_generation(&done_path), 1);
    let committed_one = project_snapshot(
        &model,
        SnapshotObservation {
            latest: 1,
            job: 1,
            payload: 1,
            done: true,
        },
    );
    assert_transition(
        &model,
        "CommitCurrent",
        &initial,
        &committed_one,
        "snapshot generation-one commit",
    );

    let second = begin_snapshot_generation(&path).expect("begin generation two");
    assert_eq!(second.generation(), 2);
    assert!(!done_path.exists(), "begin removes the durable old marker");
    let begun_two = project_snapshot(
        &model,
        SnapshotObservation {
            latest: 2,
            job: 1,
            payload: 1,
            done: false,
        },
    );
    assert_transition(
        &model,
        "BeginNew",
        &committed_one,
        &begun_two,
        "snapshot generation-two begin",
    );

    let stale = write_snapshot_artifacts(&SnapshotPng::encode(&frame_one), "stale", &first)
        .expect_err("superseded worker must fail closed");
    assert!(stale.contains("superseded"));
    assert!(!done_path.exists());
    assert_eq!(
        std::fs::read_to_string(&text_path).unwrap(),
        "generation-one"
    );
    assert_transition(
        &model,
        "CommitOld",
        &begun_two,
        &begun_two,
        "snapshot stale-worker stutter",
    );

    let selected_two = project_snapshot(
        &model,
        SnapshotObservation {
            latest: 2,
            job: 2,
            payload: 1,
            done: false,
        },
    );
    assert_transition(
        &model,
        "SelectCurrent",
        &begun_two,
        &selected_two,
        "snapshot scheduler selects current worker",
    );
    write_snapshot_artifacts(&SnapshotPng::encode(&frame_two), "generation-two", &second)
        .expect("current worker commits");
    assert_eq!(marker_generation(&done_path), 2);
    assert_eq!(
        std::fs::read_to_string(&text_path).unwrap(),
        "generation-two"
    );
    let committed_two = project_snapshot(
        &model,
        SnapshotObservation {
            latest: 2,
            job: 2,
            payload: 2,
            done: true,
        },
    );
    assert_transition(
        &model,
        "CommitCurrent",
        &selected_two,
        &committed_two,
        "snapshot generation-two commit",
    );

    let mut stale_marker = begun_two.clone();
    stale_marker.insert("payload", 1);
    stale_marker.insert("done", 1);
    reject_transition(
        &model,
        "CommitOld",
        &begun_two,
        &stale_marker,
        "snapshot stale-marker negative control",
    );
    assert!(!model.check_invariant("CommittedPayloadIsCurrent", &stale_marker));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn real_pinned_read_write_and_reply_validation_conform() {
    let root = unique_dir("anchored");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("read.bin"), b"read-inside").unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let model = anchored_artifact_transaction_model();
    let initial = project_anchored(&model, unconfined());

    let read_dir = PinnedDir::open(&root).expect("pin read directory");
    let read_pinned = project_anchored(&model, pinned());
    assert_transition(
        &model,
        "ConfinePin",
        &initial,
        &read_pinned,
        "artifact read confinement",
    );
    let (bytes, read_guard) = read_dir
        .read_private(OsStr::new("read.bin"), 64)
        .expect("handle-relative read");
    assert_eq!(bytes, b"read-inside");
    let read_done = project_anchored(&model, operated(1));
    assert_transition(
        &model,
        "ReadPinned",
        &read_pinned,
        &read_done,
        "artifact retained-handle read",
    );
    read_guard.validate_path_identity().unwrap();

    let target = ConfinedImage::for_test(&root, "write.bin");
    let write_pinned = project_anchored(&model, pinned());
    let file = target
        .write_private(b"write-inside")
        .expect("handle-relative write");
    let write_done = project_anchored(&model, operated(2));
    assert_transition(
        &model,
        "WritePinned",
        &write_pinned,
        &write_done,
        "artifact retained-handle write",
    );
    target
        .validate_for_reply(&file)
        .expect("unchanged identity authorizes reply");
    let reply_done = project_anchored(&model, replied(2));
    assert_transition(
        &model,
        "ValidateReply",
        &write_done,
        &reply_done,
        "artifact successful reply validation",
    );
    assert_eq!(
        std::fs::read(root.join("write.bin")).unwrap(),
        b"write-inside"
    );

    drop(file);
    drop(target);
    drop(read_guard);
    drop(read_dir);
    let _ = std::fs::remove_dir_all(root);
}

/// Where bytes a confined operation touched ended up, in the model's
/// `effect_target` coding: the pinned original directory (`1`), the swapped-in
/// replacement (`2`), or nowhere (`0`).
#[cfg(unix)]
fn landed(moved: &Path, outside: &Path, bytes: &[u8]) -> i64 {
    let holds = |dir: &Path| {
        std::fs::read_dir(dir).unwrap().flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && std::fs::read(entry.path()).is_ok_and(|content| content == bytes)
        })
    };
    match (holds(moved), holds(outside)) {
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 0,
        (true, true) => panic!("one write landed in both directories"),
    }
}

/// TIER-1: the swap happens between pinning and the operation. Both shipping
/// operations run through the handle retained before the swap, and where their
/// bytes went is read back off the filesystem — the retained original
/// (`effect_target = 1`) or the swapped-in outside directory (`2`). A path that
/// is resolved again after the swap, the confinement before 6aeae4606, is
/// replayed with the same reads and writes by path, and refused.
#[cfg(unix)]
#[test]
fn swapped_ancestor_fails_reply_and_outside_mutants_are_rejected() {
    use std::os::unix::fs::symlink;

    let root = unique_dir("swap");
    let outside = unique_dir("outside");
    std::fs::create_dir_all(root.join("images")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    // `PinnedDir::open` walks the path without following symlinks, so the
    // temp dir's `/var` spelling is resolved first.
    let root = std::fs::canonicalize(root).unwrap();
    let outside = std::fs::canonicalize(outside).unwrap();
    std::fs::write(root.join("images").join("read.bin"), b"read-inside").unwrap();
    std::fs::write(outside.join("read.bin"), b"read-outside").unwrap();
    let model = anchored_artifact_transaction_model();
    let initial = project_anchored(&model, unconfined());

    // Pin both transactions, then swap the ancestor under them.
    let read_pin = PinnedDir::open(&root.join("images")).expect("pin read directory");
    let write_pin = PinnedDir::open(&root.join("images")).expect("pin write directory");
    let pinned_state = project_anchored(&model, pinned());
    assert_transition(
        &model,
        "ConfinePin",
        &initial,
        &pinned_state,
        "artifact confinement",
    );
    let moved = root.join("images-moved");
    std::fs::rename(root.join("images"), &moved).unwrap();
    symlink(&outside, root.join("images")).unwrap();
    let swapped_before_io = project_anchored(
        &model,
        AnchoredObservation {
            swapped: true,
            path_identity: 2,
            ..pinned()
        },
    );
    assert_transition(
        &model,
        "SwapAncestor",
        &pinned_state,
        &swapped_before_io,
        "artifact ancestor replacement before I/O",
    );
    let after_swap = |operation: i64, effect_target: i64| {
        project_anchored(
            &model,
            AnchoredObservation {
                phase: 2,
                swapped: true,
                path_identity: 2,
                operation,
                effect_target,
                ..pinned()
            },
        )
    };
    let replied_after_swap = |operation: i64, effect_target: i64| {
        project_anchored(
            &model,
            AnchoredObservation {
                phase: 3,
                swapped: true,
                path_identity: 2,
                operation,
                effect_target,
                reply: 2,
                ..pinned()
            },
        )
    };

    // READ: the operation half of `read_private` reads through the retained
    // handle, and its reply half then fails closed; the composed call refuses.
    let (bytes, read_guard) = read_pin
        .read_private_at_retained(OsStr::new("read.bin"), 64)
        .expect("the retained handle still reads the original");
    let read_target = match bytes.as_slice() {
        b"read-inside" => 1,
        b"read-outside" => 2,
        other => panic!("unexpected read {other:?}"),
    };
    let read_done = after_swap(1, read_target);
    assert_transition(
        &model,
        "ReadPinned",
        &swapped_before_io,
        &read_done,
        "artifact retained-handle read after the swap",
    );
    read_guard
        .validate_path_identity()
        .expect_err("a swapped ancestor cannot authorize the read's reply");
    assert_transition(
        &model,
        "ValidateReply",
        &read_done,
        &replied_after_swap(1, read_target),
        "artifact fail-closed read reply",
    );
    assert!(
        read_pin.read_private(OsStr::new("read.bin"), 64).is_err(),
        "the composed shipping read fails closed"
    );
    // Negative control: the same read by path lands on the replacement.
    let resolved = std::fs::read(root.join("images").join("read.bin")).unwrap();
    assert_eq!(resolved, b"read-outside");
    let outside_read = after_swap(1, 2);
    reject_transition(
        &model,
        "ReadPinned",
        &swapped_before_io,
        &outside_read,
        "artifact re-resolved-read negative control",
    );
    assert!(!model.check_invariant("AnchoredAccessNeverOutside", &outside_read));

    // WRITE: the shipping write stages its bytes through the retained handle;
    // the authorizer runs once they are down, which is where they are measured.
    // Its final path validation then fails closed and removes them.
    let mut write_target = None;
    write_pin
        .write_private_authorized(OsStr::new("shot.png"), b"write-after-swap", || {
            write_target = Some(landed(&moved, &outside, b"write-after-swap"));
            true
        })
        .expect_err("a swapped ancestor cannot publish the write");
    let write_target = write_target.expect("the write staged its bytes");
    let write_done = after_swap(2, write_target);
    assert_transition(
        &model,
        "WritePinned",
        &swapped_before_io,
        &write_done,
        "artifact retained-handle write after the swap",
    );
    assert_transition(
        &model,
        "ValidateReply",
        &write_done,
        &replied_after_swap(2, write_target),
        "artifact fail-closed write reply",
    );
    assert_eq!(
        landed(&moved, &outside, b"write-after-swap"),
        0,
        "the refused write leaves its bytes nowhere"
    );
    // Negative control: the same write by path lands outside.
    std::fs::write(root.join("images").join("shot.png"), b"write-by-path").unwrap();
    let outside_write = after_swap(2, landed(&moved, &outside, b"write-by-path"));
    assert_eq!(outside_write["effect_target"], 2);
    reject_transition(
        &model,
        "WritePinned",
        &swapped_before_io,
        &outside_write,
        "artifact re-resolved-write negative control",
    );
    assert!(!model.check_invariant("AnchoredAccessNeverOutside", &outside_write));
    std::fs::remove_file(outside.join("shot.png")).unwrap();

    // A swap in the operation-to-reply interval: the write is published inside,
    // then the ancestor moves, and the reply fails closed.
    std::fs::remove_file(root.join("images")).unwrap();
    std::fs::rename(&moved, root.join("images")).unwrap();
    let target = ConfinedImage::for_test(&root.join("images"), "late.png");
    let file = target.write_private(b"late-inside").unwrap();
    let written = project_anchored(&model, operated(2));
    std::fs::rename(root.join("images"), &moved).unwrap();
    symlink(&outside, root.join("images")).unwrap();
    let swapped = project_anchored(
        &model,
        AnchoredObservation {
            swapped: true,
            path_identity: 2,
            ..operated(2)
        },
    );
    assert_transition(
        &model,
        "SwapAncestor",
        &written,
        &swapped,
        "artifact ancestor replacement after the write",
    );
    target
        .validate_for_reply(&file)
        .expect_err("swapped ancestor cannot authorize reply");
    assert_transition(
        &model,
        "ValidateReply",
        &swapped,
        &replied_after_swap(2, 1),
        "artifact fail-closed reply validation",
    );
    assert!(
        std::fs::read_dir(&outside)
            .unwrap()
            .all(|entry| { entry.unwrap().file_name() == "read.bin" })
    );
    assert_eq!(
        std::fs::read(moved.join("late.png")).unwrap(),
        b"late-inside"
    );

    let false_success = project_anchored(
        &model,
        AnchoredObservation {
            phase: 3,
            swapped: true,
            path_identity: 2,
            operation: 2,
            effect_target: 1,
            reply: 1,
            certified_identity: 2,
        },
    );
    reject_transition(
        &model,
        "ValidateReply",
        &swapped,
        &false_success,
        "artifact false-success reply negative control",
    );
    assert!(!model.check_invariant("SuccessfulReplyCertifiesOriginal", &false_success));

    drop(file);
    drop(target);
    drop(read_pin);
    drop(write_pin);
    let _ = std::fs::remove_file(root.join("images"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}
