// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for journal-v5 one-shot GitHub POST authority.

use crate::{publish, sign};

use aterm_spec::derive::{
    Model, release_durable_post_intent_model, release_historical_recovery_model,
};
use aterm_spec::interp::State;
use publish::{CutCtx, CutKind, DurablePostDecision, Journal};
use std::path::{Path, PathBuf};

fn journal() -> Journal {
    Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.55.0".into(),
        build_number: 55,
        commit: "a".repeat(40),
        min_build: None,
        arm64_only: false,
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        done: Vec::new(),
    }
}

fn context(root: &Path, with_journal: bool) -> CutCtx {
    let journal_path = root.join("nested/dist/cut-state.toml");
    CutCtx {
        verify_pubkey: None,
        // No credentials: this model exercises journal/state transitions, not signing.
        credentials: None,
        // Tier APPLE inactive — NOT because the shipped anchor is (it has been armed
        // since 2026-08-15, which is what this line used to claim), but because this
        // model covers the
        // one-shot POST intents; a resolved Apple tier would be a certificate
        // this test has no business needing.
        apple: sign::AppleTier::Inactive,
        repo: root.to_path_buf(),
        tree: root.to_path_buf(),
        dist: root.join("nested/dist"),
        journal_path,
        slug: "owner/repo".into(),
        version: "0.55.0".into(),
        tag: "v0.55.0".into(),
        build: 55,
        commit: "a".repeat(40),
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        linux: None,
        signature_required: false,
        signature_pubkey: None,
        // Unattributed, as a FORK's cut is: with no master pinned no roster authorizes
        // anything and no roster asset ships. WRONG BEFORE: "as every cut from this tree
        // is" — this tree's master has been armed since 2026-08-15.
        signature_machine_id: None,
        attribution: None,
        roster: None,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        // No public update channel in this fixture: the model covers the
        // PRIVATE side's one-shot POST intents, and the mirror's twin set must
        // start empty so a converged private upload cannot be mistaken for
        // authority on the channel.
        mirror_slug: None,
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        kind: CutKind::Real,
        no_paint_smoke: false,
        lease: None,
        fence: None,
        notes_section: "0.55.0".into(),
        journal: with_journal.then(journal),
    }
}

fn step(model: &Model, state: &mut aterm_spec::interp::State, action: &str, label: &str) {
    let before = state.clone();
    assert!(model.fire(action, state), "model disabled {action}");
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[],
        &before,
        state,
        Some(action),
        label,
    );
    assert!(admitted, "model rejected {label}: {why}");
}

#[test]
fn real_guard_and_fsynced_journal_refine_one_shot_model() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("durable-post-intent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let model = release_durable_post_intent_model();
    let mut state = model.init_state();
    let mut ctx = context(&root, true);

    assert_eq!(
        publish::durable_post_decision(false, false),
        DurablePostDecision::PersistIntentThenPost
    );
    step(
        &model,
        &mut state,
        "PersistCreateIntent",
        "durable create intent persisted before POST permit",
    );
    let permit = ctx.persist_draft_create_intent().unwrap();
    let loaded = Journal::load(&ctx.journal_path).unwrap().unwrap();
    assert!(loaded.draft_create_issued);
    assert!(ctx.journal_path.parent().unwrap().is_dir());
    drop(permit);

    // Crash before POST destroys only the process-local permit. The real guard
    // loaded from disk and the model both refuse to mint/issue another one.
    step(&model, &mut state, "Crash", "crash destroys create permit");
    step(
        &model,
        &mut state,
        "Resume",
        "resume retains durable intent",
    );
    assert_eq!(
        publish::durable_post_decision(loaded.draft_create_issued, false),
        DurablePostDecision::AwaitVisibility
    );
    assert!(!model.action_enabled("IssueCreatePost", &state));
    assert!(ctx.persist_draft_create_intent().is_err());

    ctx.release_id = Some(55);
    let persisted = ctx.journal.as_mut().unwrap();
    persisted.release_id = Some(55);
    persisted.save(&ctx.journal_path).unwrap();
    let upload_permit = ctx.persist_upload_intent("aterm-0.55.0.dmg").unwrap();
    drop(upload_permit);
    let uploaded = Journal::load(&ctx.journal_path).unwrap().unwrap();
    assert_eq!(uploaded.upload_intents, ["aterm-0.55.0.dmg"]);
    assert!(ctx.persist_upload_intent("aterm-0.55.0.dmg").is_err());

    // A real publication context without a journal cannot mint authority.
    let mut unjournaled = context(&root, false);
    assert!(unjournaled.persist_draft_create_intent().is_err());
    assert!(
        unjournaled
            .persist_upload_intent("aterm-0.55.0.dmg")
            .is_err()
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lost_response_converges_visible_create_and_upload_without_repost() {
    let model = release_durable_post_intent_model();
    let mut state = model.init_state();
    step(&model, &mut state, "PersistCreateIntent", "persist create");
    step(&model, &mut state, "IssueCreatePost", "one create POST");
    step(&model, &mut state, "Crash", "create response lost");
    step(&model, &mut state, "Resume", "resume create");
    assert_eq!(
        publish::durable_post_decision(true, false),
        DurablePostDecision::AwaitVisibility
    );
    assert!(!model.action_enabled("IssueCreatePost", &state));
    // NEGATIVE CONTROL: the real decision converges only on a visible object, so
    // a landed POST whose answer was lost waits. Taking that POST at its word
    // converges a draft nobody has re-listed.
    assert!(!model.action_enabled("ConvergeCreatedDraft", &state));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let trusted = buggy.successors("ConvergeCreatedDraft", &state)[0].clone();
    assert!(!buggy.check_invariant("CreateConvergenceRequiresVisibility", &trusted));
    step(
        &model,
        &mut state,
        "RevealCreatedDraft",
        "delayed draft visible",
    );
    assert_eq!(
        publish::durable_post_decision(true, true),
        DurablePostDecision::ConvergeVisible
    );
    step(
        &model,
        &mut state,
        "ConvergeCreatedDraft",
        "exact draft convergence",
    );

    step(&model, &mut state, "PersistUploadIntent", "persist upload");
    step(&model, &mut state, "IssueUploadPost", "one upload POST");
    step(&model, &mut state, "Crash", "upload response lost");
    step(&model, &mut state, "Resume", "resume upload");
    assert!(!model.action_enabled("IssueUploadPost", &state));
    step(
        &model,
        &mut state,
        "RevealUploadedAsset",
        "delayed asset visible",
    );
    step(
        &model,
        &mut state,
        "ConvergeUploadedAsset",
        "exact asset convergence",
    );

    // Explicit negative control for the retired retry policy.
    let mut retry = buggy.init_state();
    assert!(buggy.fire("PersistCreateIntent", &mut retry));
    assert!(buggy.fire("Crash", &mut retry));
    assert!(buggy.fire("Resume", &mut retry));
    assert!(buggy.fire("IssueCreatePost", &mut retry));
    assert!(!buggy.check_invariant("LostCreatePermitCannotPost", &retry));
}

#[test]
fn absent_cleanup_selector_is_fail_closed_for_issued_or_unknown_intent() {
    assert_eq!(
        publish::absent_draft_decision(Some(false), false),
        publish::AbsentDraftDecision::AbandonProvenNoPost
    );
    for knowledge in [Some(true), None] {
        assert_eq!(
            publish::absent_draft_decision(knowledge, false),
            publish::AbsentDraftDecision::RetainOwnerAwaitVisibility
        );
    }
    // The operator answers for a LOST journal only. A journal that PROVES a POST was
    // issued still wins — there, delayed visibility is the only explanation left.
    assert_eq!(
        publish::absent_draft_decision(None, true),
        publish::AbsentDraftDecision::AbandonProvenNoPost
    );
    assert_eq!(
        publish::absent_draft_decision(Some(true), true),
        publish::AbsentDraftDecision::RetainOwnerAwaitVisibility
    );
    use publish::DraftCleanupDecision::{
        AbandonProvenNoPost, DeleteIssuedVisible, RefuseUnknownOrInconsistent,
        RetainIssuedAwaitVisibility,
    };
    // `bound` = the visible draft targets the recovery claim's commit, i.e. the remote
    // itself proves the object is this claim's. It is the ONLY thing that may stand in
    // for a lost journal, and only when a draft is actually visible.
    for (knowledge, visible, bound, expected) in [
        (Some(false), false, false, AbandonProvenNoPost),
        (Some(false), true, false, RefuseUnknownOrInconsistent),
        (Some(false), true, true, RefuseUnknownOrInconsistent),
        (Some(true), false, false, RetainIssuedAwaitVisibility),
        (Some(true), true, false, DeleteIssuedVisible),
        (None, false, false, RefuseUnknownOrInconsistent),
        (None, false, true, RefuseUnknownOrInconsistent),
        (None, true, false, RefuseUnknownOrInconsistent),
        // The cross-machine recovery this exists for.
        (None, true, true, DeleteIssuedVisible),
    ] {
        assert_eq!(
            publish::draft_cleanup_decision(knowledge, visible, bound),
            expected,
            "cleanup matrix drift for {knowledge:?}, visible={visible}, bound={bound}"
        );
    }

    let model = release_durable_post_intent_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut duplicated = buggy.init_state();
    assert!(buggy.fire("PersistCreateIntent", &mut duplicated));
    assert!(buggy.fire("IssueCreatePost", &mut duplicated));
    assert!(buggy.fire("Crash", &mut duplicated));
    assert!(buggy.fire("Resume", &mut duplicated));
    assert!(buggy.fire("IssueCreatePost", &mut duplicated));
    assert!(!buggy.check_invariant("CreatePostIsOneShot", &duplicated));
}

const RECOVERY_TAG: &str = "v0.55.0";
const RECOVERY_CLAIM: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// Project one recovery observation onto `ReleaseHistoricalRecovery`: what the
/// journal knows (`None` = lost), and whether a draft is visible and bound to the
/// claim's commit — the shipping `claim_bound`, which only a visible draft has.
fn recovery_state(model: &Model, knowledge: Option<bool>, visible: bool, bound: bool) -> State {
    let mut state = model.init_state();
    let learned = match knowledge {
        Some(false) => Some("LearnNoPostFromCurrentJournal"),
        Some(true) => Some("LearnIssuedIntentFromCurrentJournal"),
        None => None,
    };
    for action in learned.into_iter().chain(visible.then_some(if bound {
        "ObserveExactDraft"
    } else {
        "ObserveUnboundDraft"
    })) {
        let before = state.clone();
        assert!(model.fire(action, &mut state), "{action}");
        assert_eq!(
            aterm_spec::interp::admits(model, &before, &state),
            Some(action)
        );
    }
    state
}

/// The shipping delete authority for one row: `delete_owned_draft_release`
/// deletes only when `draft_cleanup_decision` says so AND the draft it found
/// passes `validate_release_object_capability` against the claim's commit.
fn real_deletes(knowledge: Option<bool>, visible: bool, bound: bool) -> bool {
    let draft = publish::ReleaseObjectIdentity {
        id: 7,
        tag: RECOVERY_TAG.to_string(),
        draft: true,
        target_commitish: if bound { RECOVERY_CLAIM } else { OTHER_COMMIT }.to_string(),
    };
    publish::draft_cleanup_decision(knowledge, visible, bound)
        == publish::DraftCleanupDecision::DeleteIssuedVisible
        && publish::validate_release_object_capability(
            Some(&draft),
            7,
            RECOVERY_TAG,
            RECOVERY_CLAIM,
            true,
        )
        .is_ok()
}

/// The healthy model's delete authority at a projected state: `DeleteExactDraft`
/// now, or once the remote's claim binding has answered a lost journal. Each step
/// taken is checked to be the model transition it claims to be.
fn model_deletes(model: &Model, state: &State) -> bool {
    let mut paths = vec![state.clone()];
    paths.extend(model.successors("LearnIssuedIntentFromClaimBinding", state));
    paths.iter().any(|before| {
        model
            .successors("DeleteExactDraft", before)
            .iter()
            .any(|after| {
                aterm_spec::interp::admits(model, before, after) == Some("DeleteExactDraft")
            })
    })
}

/// Tier-1 for `ReleaseHistoricalRecovery`: every row of the shipping cleanup
/// selector — `draft_cleanup_decision` composed with the capability check the
/// delete runs, and `absent_draft_decision` with and without the operator's
/// `--no-draft-was-posted` — is projected onto the model, and the model's healthy
/// actions admit exactly what the shipping code does. The mutants are replayed on
/// the rows the shipping code refuses.
#[test]
fn recovery_cleanup_selector_refines_the_historical_recovery_model() {
    let model = release_historical_recovery_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut unbound_lost_journal = false;
    let mut unbound_issued = false;
    for knowledge in [Some(false), Some(true), None] {
        for (visible, bound) in [(false, false), (true, false), (true, true)] {
            let state = recovery_state(&model, knowledge, visible, bound);
            let real = real_deletes(knowledge, visible, bound);
            assert_eq!(
                model_deletes(&model, &state),
                real,
                "delete authority for {knowledge:?}, visible={visible}, bound={bound}"
            );
            if visible {
                // A visible draft is never abandoned around: the absent-listing
                // lane is the only abandon, and it is not this one.
                assert!(!model.action_enabled("AbandonProvenNoPost", &state));
                assert!(!model.action_enabled("AbandonOnOperatorNoPostAnswer", &state));
            }
            // NEGATIVE CONTROLS on the rows the shipping code refuses to delete.
            if visible && !bound {
                assert!(!real);
                if knowledge.is_none() {
                    // The lost-journal row with its `claim_bound` conjunct dropped.
                    let deleted = buggy.successors("DeleteUnknownDraft", &state)[0].clone();
                    assert!(!buggy.check_invariant("DraftDeletionRequiresIssuedIntent", &deleted));
                    unbound_lost_journal = true;
                }
                if knowledge == Some(true) {
                    // `draft_cleanup_decision` alone says delete here; only the
                    // capability check refuses, and skipping it deletes someone
                    // else's draft.
                    assert_eq!(
                        publish::draft_cleanup_decision(knowledge, visible, bound),
                        publish::DraftCleanupDecision::DeleteIssuedVisible
                    );
                    let deleted = buggy
                        .successors("DeleteIssuedDraftWithoutCapabilityCheck", &state)[0]
                        .clone();
                    assert!(!buggy.check_invariant("DeletedDraftTargetsTheClaim", &deleted));
                    unbound_issued = true;
                }
            }
        }
    }
    assert!(unbound_lost_journal && unbound_issued);

    // The absent listing: abandon on a journal that proves no POST, or on the
    // operator's answer for a LOST journal — never over a journal proving one.
    for knowledge in [Some(false), Some(true), None] {
        let state = recovery_state(&model, knowledge, false, false);
        for operator_asserts_no_post in [false, true] {
            let real = publish::absent_draft_decision(knowledge, operator_asserts_no_post)
                == publish::AbsentDraftDecision::AbandonProvenNoPost;
            let mut abandons = model.action_enabled("AbandonProvenNoPost", &state);
            if operator_asserts_no_post {
                abandons |= model.action_enabled("AbandonOnOperatorNoPostAnswer", &state);
            }
            assert_eq!(
                abandons, real,
                "absent listing for {knowledge:?}, operator answer={operator_asserts_no_post}"
            );
        }
        match knowledge {
            // The answer folded into `--old-publisher-stopped`: unlock with no one
            // answering for the lost journal.
            None => {
                let unlocked = buggy.successors("AbandonUnknownAbsent", &state)[0].clone();
                assert!(!buggy.check_invariant("AmbiguousAbsenceRetainsOwner", &unlocked));
            }
            // The answer overriding a journal that proves a POST.
            Some(true) => {
                let unlocked = buggy.successors("AbandonIssuedAbsent", &state)[0].clone();
                assert!(!buggy.check_invariant("AmbiguousAbsenceRetainsOwner", &unlocked));
            }
            Some(false) => {}
        }
    }

    // The owner's lease is released only after the tag cleanup. The order lives in
    // `recover_under_fence`, whose every branch runs against GitHub, so it is
    // asserted as source (the idiom of `every_release_body_post_reads_through_the_bound`).
    let src = include_str!("../../src/publish.rs");
    let start = src
        .find("fn recover_under_fence(")
        .expect("recovery present");
    let body = &src[start..];
    let body = &body[..body.find("\nfn ").unwrap_or(body.len())];
    let tag_deleted = body
        .find("delete_owned_release_tag(&git, &tag, owner, &lease, fence)?;")
        .expect("the tag cleanup");
    let released = body
        .find("release_completed_publisher_session(&git, owner, fence)?;")
        .expect("the owner release");
    assert!(
        tag_deleted < released,
        "the owner is released only after its tag is gone"
    );
    let mut cleaned = recovery_state(&model, Some(true), true, true);
    assert!(model.fire("DeleteExactDraft", &mut cleaned));
    assert!(!model.action_enabled("ReleaseOwnerBeforeTagCleanup", &cleaned));
    let early = buggy.successors("ReleaseOwnerBeforeTagCleanup", &cleaned)[0].clone();
    assert!(!buggy.check_invariant("CompletionReleasesOwner", &early));
}

#[test]
fn curl_transport_preflight_requires_every_no_retry_post_option() {
    // `--upload-file` is in this set because the asset leg STREAMS: a
    // batteries-included DMG is over a gigabyte and `--data-binary @file`
    // buffers it whole (2026-08-19, `out of memory` after a full notarized
    // build). A curl that cannot stream must fail the preflight, not the upload.
    let help = "--data-binary --fail-with-body --header --request --retry \
                --show-error --silent --upload-file --url";
    publish::validate_one_shot_curl_help(help).unwrap();
    for missing in help.split_whitespace() {
        let mutant = help.replace(missing, "");
        assert!(
            publish::validate_one_shot_curl_help(&mutant).is_err(),
            "missing {missing} must fail before durable intent"
        );
    }
}

#[test]
fn one_shot_auth_token_is_pinned_to_the_public_github_host() {
    assert_eq!(publish::GITHUB_AUTH_HOST, "github.com");
    assert_eq!(
        publish::github_auth_token_args(),
        ["auth", "token", "--hostname", "github.com"]
    );
    assert_eq!(publish::GITHUB_API_ORIGIN, "https://api.github.com");
    assert_eq!(publish::GITHUB_UPLOAD_ORIGIN, "https://uploads.github.com");
    assert!(publish::GITHUB_API_ORIGIN.ends_with(publish::GITHUB_AUTH_HOST));
    assert!(publish::GITHUB_UPLOAD_ORIGIN.ends_with(publish::GITHUB_AUTH_HOST));
}

#[test]
fn strict_draft_delete_never_converges_on_pre_delete_absence() {
    assert!(!publish::exact_delete_absence_is_converged(false, false));
    assert!(publish::exact_delete_absence_is_converged(false, true));
    assert!(publish::exact_delete_absence_is_converged(true, false));
    assert!(publish::exact_delete_absence_is_converged(true, true));
}
