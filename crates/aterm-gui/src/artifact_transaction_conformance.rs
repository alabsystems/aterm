// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for fixed-path snapshot generations and handle-anchored
//! artifacts.
//!
//! The tests drive the shipping generation fence and `PinnedDir` accessors,
//! project their observable states onto the derived models, and validate every
//! concrete transition. Deliberately stale/outside post-states are rejected as
//! non-vacuous controls.

#![cfg(test)]

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_render::Frame;
use aterm_spec::derive::{
    Model, anchored_artifact_transaction_model, artifact_reader_lease_model,
    artifact_reply_publication_model, snapshot_generation_commit_model,
    video_batch_publication_durability_model,
};
use aterm_spec::interp::State;
use aterm_spec::verify::validate_transition_tiered;

use crate::app_introspect::{begin_snapshot_generation, write_snapshot_artifacts};
use crate::control_auth::ConfinedImage;
use crate::pinned_dir::PinnedDir;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AnchoredObservation {
    phase: i64,
    pinned: bool,
    swapped: bool,
    path_identity: i64,
    operation: i64,
    effect_target: i64,
    validated: bool,
    reply: i64,
    certified_identity: i64,
}

pub(crate) fn project_anchored(model: &Model, observed: AnchoredObservation) -> State {
    let mut state = model.init_state();
    state.insert("phase", observed.phase);
    state.insert("pinned", i64::from(observed.pinned));
    state.insert("swapped", i64::from(observed.swapped));
    state.insert("path_identity", observed.path_identity);
    state.insert("operation", observed.operation);
    state.insert("effect_target", observed.effect_target);
    state.insert("validated", i64::from(observed.validated));
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

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactReplyObservation {
    phase: i64,
    artifact: bool,
    guard: bool,
    committed: bool,
    reply: bool,
    challenge: bool,
    ack: bool,
    ack_failed: bool,
    write_error: bool,
    quarantine: bool,
    quarantine_age: i64,
    expired: bool,
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
    state.insert("quarantine", i64::from(observed.quarantine));
    state.insert("quarantine_age", observed.quarantine_age);
    state.insert("expired", i64::from(observed.expired));
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
/// observations are: registry `count` -> `leases`, requested callback ->
/// `armed`, retained recording-identity disagreement -> `identity_mismatch`, the
/// last-release handoff -> `pending`, callback execution -> `sweeping`, and
/// completed callback -> `swept`. `replacement_joined` is a negative-control
/// witness and is always false for shipping code.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArtifactReaderObservation {
    leases: i64,
    armed: bool,
    pending: bool,
    sweeping: bool,
    swept: bool,
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
    state.insert("pending", i64::from(observed.pending));
    state.insert("sweeping", i64::from(observed.sweeping));
    state.insert("swept", i64::from(observed.swept));
    state.insert("identity_mismatch", i64::from(observed.identity_mismatch));
    state.insert("replacement_joined", i64::from(observed.replacement_joined));
    state
}

fn unconfined() -> AnchoredObservation {
    AnchoredObservation {
        phase: 0,
        pinned: false,
        swapped: false,
        path_identity: 0,
        operation: 0,
        effect_target: 0,
        validated: false,
        reply: 0,
        certified_identity: 0,
    }
}

fn pinned() -> AnchoredObservation {
    AnchoredObservation {
        phase: 1,
        pinned: true,
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
        validated: true,
        reply: 1,
        certified_identity: 1,
        ..pinned()
    }
}

fn assert_transition(model: &Model, action: &str, before: &State, after: &State, label: &str) {
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

fn reject_transition(model: &Model, action: &str, before: &State, after: &State, label: &str) {
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

#[allow(dead_code)]
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
    action = "BuggyAcquireReplacedIdentity",
    reason = "Buggy=1 negative control only; a mismatched retained recording identity is rejected \
              without joining the existing lexical-path lease group"
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
            "BuggyAcceptPreChallengeAck",
            "BuggyDropBeforeWrite",
            "BuggyPruneLeased",
            "BuggyPublishAfterCancel",
            "BuggyReleaseQuarantineEarly",
            "BuggyReleaseWithoutAck",
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
    assert_eq!(batch_waivers, BTreeSet::from(["BuggyPublishBeforeSync"]));

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

#[test]
fn artifact_reply_projection_conforms_through_ack_quarantine_and_failure_release() {
    let model = artifact_reply_publication_model();
    let idle = project_artifact_reply(&model, ArtifactReplyObservation::default());
    let authorized = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 2,
            artifact: true,
            guard: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let queued = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 3,
            artifact: true,
            guard: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let prepared = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 4,
            artifact: true,
            guard: true,
            committed: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let written = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 5,
            artifact: true,
            guard: true,
            committed: true,
            reply: true,
            challenge: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let peer_acked = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 6,
            artifact: true,
            guard: true,
            committed: true,
            reply: true,
            challenge: true,
            ack: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let released_after_ack = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 10,
            artifact: true,
            guard: false,
            committed: true,
            reply: true,
            challenge: true,
            ack: true,
            ..ArtifactReplyObservation::default()
        },
    );
    for (action, before, after) in [
        ("AuthorizeCommit", &idle, &authorized),
        ("QueueGuard", &authorized, &queued),
        ("PrepareWire", &queued, &prepared),
        ("WriteWire", &prepared, &written),
        ("AcknowledgePeer", &written, &peer_acked),
        ("ReleaseGuard", &peer_acked, &released_after_ack),
    ] {
        assert_transition(
            &model,
            action,
            before,
            after,
            "artifact reply queue-to-ack lifecycle",
        );
    }

    assert_transition(
        &model,
        "RetentionSweep",
        &queued,
        &queued,
        "a sibling retention sweep skips the queued artifact lease",
    );
    let pruned_while_queued = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 3,
            guard: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "RetentionSweep",
        &queued,
        &pruned_while_queued,
        "retention cannot prune a queued artifact lease",
    );

    let ack_failed = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 7,
            artifact: true,
            guard: true,
            committed: true,
            reply: true,
            challenge: true,
            ack_failed: true,
            quarantine: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let quarantine_tick_one = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 7,
            artifact: true,
            guard: true,
            committed: true,
            reply: true,
            challenge: true,
            ack_failed: true,
            quarantine: true,
            quarantine_age: 1,
            ..ArtifactReplyObservation::default()
        },
    );
    let quarantine_expired = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 8,
            artifact: true,
            guard: true,
            committed: true,
            reply: true,
            challenge: true,
            ack_failed: true,
            quarantine_age: 1,
            expired: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let released_after_ack_failure = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 11,
            artifact: true,
            committed: true,
            reply: true,
            challenge: true,
            ack_failed: true,
            quarantine_age: 1,
            expired: true,
            ..ArtifactReplyObservation::default()
        },
    );
    assert_transition(
        &model,
        "AcknowledgeFailed",
        &written,
        &ack_failed,
        "EOF, timeout, or malformed ACK transfers ownership to quarantine",
    );
    assert_transition(
        &model,
        "AdvanceQuarantine",
        &ack_failed,
        &quarantine_tick_one,
        "central quarantine advances without blocking the connection worker",
    );
    assert_transition(
        &model,
        "ExpireQuarantine",
        &quarantine_tick_one,
        &quarantine_expired,
        "the reaper removes only an entry whose 30-second deadline is due",
    );
    assert_transition(
        &model,
        "ReleaseGuard",
        &quarantine_expired,
        &released_after_ack_failure,
        "failed ACK releases only after central-quarantine expiry",
    );

    let abort_pending = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 9,
            artifact: true,
            guard: true,
            ..ArtifactReplyObservation::default()
        },
    );
    for (action, before) in [
        ("AbortAuthorized", &authorized),
        ("AbortQueued", &queued),
        ("PrepareFailed", &queued),
    ] {
        assert_transition(
            &model,
            action,
            before,
            &abort_pending,
            "pre-wire abort keeps ownership until cleanup",
        );
    }
    let released_abort = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 12,
            ..ArtifactReplyObservation::default()
        },
    );
    assert_transition(
        &model,
        "ReleaseGuard",
        &abort_pending,
        &released_abort,
        "pre-wire abort removes the unpublished artifact",
    );

    let write_failed = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 7,
            artifact: true,
            guard: true,
            committed: true,
            write_error: true,
            quarantine: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let write_quarantine_due = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 7,
            artifact: true,
            guard: true,
            committed: true,
            write_error: true,
            quarantine: true,
            quarantine_age: 1,
            ..ArtifactReplyObservation::default()
        },
    );
    let write_quarantine_expired = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 8,
            artifact: true,
            guard: true,
            committed: true,
            write_error: true,
            quarantine_age: 1,
            expired: true,
            ..ArtifactReplyObservation::default()
        },
    );
    let released_write_failure = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 11,
            artifact: true,
            committed: true,
            write_error: true,
            quarantine_age: 1,
            expired: true,
            ..ArtifactReplyObservation::default()
        },
    );
    assert_transition(
        &model,
        "WriteFailed",
        &prepared,
        &write_failed,
        "a partial socket write enters quarantine because path bytes may be visible",
    );
    assert_transition(
        &model,
        "AdvanceQuarantine",
        &write_failed,
        &write_quarantine_due,
        "the reaper observes the partial-write quarantine deadline",
    );
    assert_transition(
        &model,
        "ExpireQuarantine",
        &write_quarantine_due,
        &write_quarantine_expired,
        "the due partial-write entry expires centrally",
    );
    assert_transition(
        &model,
        "ReleaseGuard",
        &write_quarantine_expired,
        &released_write_failure,
        "partial write failure releases only after quarantine expiry",
    );

    let cancelled = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 1,
            ..ArtifactReplyObservation::default()
        },
    );
    assert_transition(
        &model,
        "Cancel",
        &idle,
        &cancelled,
        "timeout wins before publication",
    );

    let published_after_cancel = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 1,
            artifact: true,
            committed: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "AuthorizeCommit",
        &cancelled,
        &published_after_cancel,
        "cancelled publication cannot be revived",
    );
    let dropped_before_write = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 3,
            artifact: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "QueueGuard",
        &authorized,
        &dropped_before_write,
        "queue handoff cannot drop the exact artifact guard",
    );
    let ack_before_challenge = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 6,
            artifact: true,
            guard: true,
            committed: true,
            ack: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "AcknowledgePeer",
        &prepared,
        &ack_before_challenge,
        "a pre-pipelined acknowledgement cannot precede the causal nonce challenge",
    );
    let silent_release = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 10,
            artifact: true,
            committed: true,
            reply: true,
            challenge: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "ReleaseGuard",
        &written,
        &silent_release,
        "a complete reply cannot release without the matching nonce ACK",
    );
    let early_quarantine_release = project_artifact_reply(
        &model,
        ArtifactReplyObservation {
            phase: 11,
            artifact: true,
            committed: true,
            reply: true,
            challenge: true,
            ack_failed: true,
            ..ArtifactReplyObservation::default()
        },
    );
    reject_transition(
        &model,
        "ReleaseGuard",
        &ack_failed,
        &early_quarantine_release,
        "failed or half-closed clients retain the guard for the full quarantine",
    );
}

#[test]
fn artifact_reader_projection_conforms_to_last_release_sweep_lifecycle() {
    let model = artifact_reader_lease_model();
    let idle = project_artifact_reader_lease(&model, ArtifactReaderObservation::default());
    let reader_one = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            ..ArtifactReaderObservation::default()
        },
    );
    let reader_two = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 2,
            ..ArtifactReaderObservation::default()
        },
    );
    let replaced_one = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            identity_mismatch: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let armed_two = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 2,
            armed: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let armed_one = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            armed: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let pending = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            armed: true,
            pending: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let sweeping = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            armed: true,
            sweeping: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let swept = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            swept: true,
            ..ArtifactReaderObservation::default()
        },
    );
    let reacquired = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            swept: true,
            ..ArtifactReaderObservation::default()
        },
    );

    for (action, before, after, label) in [
        (
            "Acquire",
            &idle,
            &reader_one,
            "first producer-or-reader lease acquisition",
        ),
        (
            "Acquire",
            &reader_one,
            &reader_two,
            "shared producer-or-reader lease acquisition",
        ),
        (
            "Arm",
            &reader_two,
            &armed_two,
            "marker publication or final reader identity validation",
        ),
        (
            "Release",
            &armed_two,
            &armed_one,
            "non-final producer-or-reader lease release",
        ),
        (
            "Release",
            &armed_one,
            &pending,
            "final producer-or-reader lease schedules convergence",
        ),
        (
            "StartSweep",
            &pending,
            &sweeping,
            "last-release convergence begins",
        ),
        (
            "FinishSweep",
            &sweeping,
            &swept,
            "convergence completion reopens the registry",
        ),
        (
            "Acquire",
            &swept,
            &reacquired,
            "producer-or-reader lease acquisition after completed convergence",
        ),
    ] {
        assert_transition(&model, action, before, after, label);
    }
    assert_transition(
        &model,
        "RejectAcquireWhileSweeping",
        &pending,
        &pending,
        "acquisition fails closed across the last-release handoff",
    );
    assert_transition(
        &model,
        "RejectAcquireWhileSweeping",
        &sweeping,
        &sweeping,
        "acquisition fails closed while the sweep owns the registry",
    );
    assert_transition(
        &model,
        "ReplaceIdentity",
        &reader_one,
        &replaced_one,
        "an external same-name replacement changes only the candidate identity",
    );
    assert_transition(
        &model,
        "RejectReplacedIdentity",
        &replaced_one,
        &replaced_one,
        "a replacement cannot join the live recording lease group",
    );

    let early_sweep = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            armed: true,
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
    let acquired_during_pending = project_artifact_reader_lease(
        &model,
        ArtifactReaderObservation {
            leases: 1,
            armed: true,
            pending: true,
            ..ArtifactReaderObservation::default()
        },
    );
    reject_transition(
        &model,
        "Acquire",
        &pending,
        &acquired_during_pending,
        "no producer-or-reader lease may enter the last-release/sweep interval",
    );
    let joined_replacement = project_artifact_reader_lease(
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
        &joined_replacement,
        "ordinary acquisition cannot admit a replaced recording identity",
    );
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

    write_snapshot_artifacts(&frame_one, "generation-one", &first).expect("commit generation one");
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

    let stale = write_snapshot_artifacts(&frame_one, "stale", &first)
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
    write_snapshot_artifacts(&frame_two, "generation-two", &second)
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

#[cfg(unix)]
#[test]
fn swapped_ancestor_fails_reply_and_outside_mutants_are_rejected() {
    use std::os::unix::fs::symlink;

    let root = unique_dir("swap");
    let outside = unique_dir("outside");
    std::fs::create_dir_all(root.join("images")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let target = ConfinedImage::for_test(&root.join("images"), "shot.png");
    let file = target.write_private(b"inside").unwrap();
    let model = anchored_artifact_transaction_model();
    let written = project_anchored(&model, operated(2));

    let moved = root.join("images-moved");
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
        "artifact ancestor replacement",
    );
    target
        .validate_for_reply(&file)
        .expect_err("swapped ancestor cannot authorize reply");
    let failed = project_anchored(
        &model,
        AnchoredObservation {
            phase: 3,
            swapped: true,
            path_identity: 2,
            operation: 2,
            effect_target: 1,
            validated: true,
            reply: 2,
            ..pinned()
        },
    );
    assert_transition(
        &model,
        "ValidateReply",
        &swapped,
        &failed,
        "artifact fail-closed reply validation",
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    assert_eq!(std::fs::read(moved.join("shot.png")).unwrap(), b"inside");

    let phase_one_swapped = project_anchored(
        &model,
        AnchoredObservation {
            swapped: true,
            path_identity: 2,
            ..pinned()
        },
    );
    let outside_read = project_anchored(
        &model,
        AnchoredObservation {
            phase: 2,
            swapped: true,
            path_identity: 2,
            operation: 1,
            effect_target: 2,
            ..pinned()
        },
    );
    reject_transition(
        &model,
        "ReadPinned",
        &phase_one_swapped,
        &outside_read,
        "artifact re-resolved-read negative control",
    );
    assert!(!model.check_invariant("AnchoredAccessNeverOutside", &outside_read));

    let false_success = project_anchored(
        &model,
        AnchoredObservation {
            phase: 3,
            swapped: true,
            path_identity: 2,
            operation: 2,
            effect_target: 1,
            validated: true,
            reply: 1,
            certified_identity: 2,
            ..pinned()
        },
    );
    reject_transition(
        &model,
        "ValidateReply",
        &swapped,
        &false_success,
        "artifact false-success reply negative control",
    );
    assert!(!model.check_invariant("SwappedPathNeverCertified", &false_success));

    drop(file);
    drop(target);
    let _ = std::fs::remove_file(root.join("images"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}
