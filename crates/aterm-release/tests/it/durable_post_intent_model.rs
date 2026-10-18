// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the release journal's one-shot GitHub POST authority.

use crate::{ledger, publish, sign};

use aterm_models_release::{release_durable_post_intent_model, release_historical_recovery_model};
use aterm_spec::derive::Model;
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
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        done: Vec::new(),
    }
}

fn context(root: &Path, with_journal: bool) -> CutCtx {
    let journal_path = root.join("nested/dist/cut-state.toml");
    CutCtx {
        verify_pubkey: None,
        // No credentials: journal/state transitions only, no signing.
        credentials: None,
        // The Apple tier is inactive because this model covers only the one-shot POST intents.
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
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        manifest_signed: false,
        linux: None,
        signature_required: false,
        signature_pubkey: None,
        // Unattributed, as a fork's cut is: with no master pinned, no roster authorizes
        // anything and none ships.
        signature_machine_id: None,
        attribution: None,
        roster: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        kind: CutKind::Real,
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
    let permit = ctx.persist_release_intent().unwrap();
    let loaded = Journal::load(&ctx.journal_path).unwrap().unwrap();
    assert!(loaded.release_intent);
    assert!(ctx.journal_path.parent().unwrap().is_dir());
    drop(permit);

    // A crash before the POST destroys only the process-local permit; the guard loaded from
    // disk and the model both refuse another.
    step(&model, &mut state, "Crash", "crash destroys create permit");
    step(
        &model,
        &mut state,
        "Resume",
        "resume retains durable intent",
    );
    assert_eq!(
        publish::durable_post_decision(loaded.release_intent, false),
        DurablePostDecision::AwaitVisibility
    );
    assert!(!model.action_enabled("IssueCreatePost", &state));
    assert!(ctx.persist_release_intent().is_err());

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
    assert!(unjournaled.persist_release_intent().is_err());
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
    // Negative control: the real decision converges only on a visible object, so a landed POST
    // whose answer was lost waits; taking it at its word converges an unlisted draft.
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

    // Negative control: a resume that posts again after a crash.
    let mut retry = buggy.init_state();
    assert!(buggy.fire("PersistCreateIntent", &mut retry));
    assert!(buggy.fire("Crash", &mut retry));
    assert!(buggy.fire("Resume", &mut retry));
    assert!(buggy.fire("IssueCreatePost", &mut retry));
    assert!(!buggy.check_invariant("LostCreatePermitCannotPost", &retry));
}

/// An upload intent is spent only on a POST that provably created nothing, over the real
/// journal and the derived model. This drives `CutCtx::settle_upload_intent` with curl's
/// answers and asks what a resume asks next: may the asset be POSTed again?
///
/// A POST curl never sent (exit 2) or the server answered with a 4xx created nothing: its
/// intent is retracted and the resume may post once more. Everything else can hide a stored
/// object and keeps its intent, so the resume decides `AwaitVisibility` and mints no permit.
/// Each row is projected onto `ReleaseDurablePostIntent` (`IssueRefusedUploadPost` or
/// `IssueUploadPost`), whose `RetractUploadIntent` is enabled for exactly the rows the real
/// settle retracts; a blanket retraction, replayed on the mutant, leaves an object no intent
/// names.
#[test]
fn an_upload_intent_is_freed_by_a_post_that_created_nothing_and_by_nothing_else() {
    use publish::PostOutcome::{NeverSent, Refused, Unknown};
    const NAME: &str = "aterm-0.55.0.dmg";
    const OTHER: &str = "aterm-0.55.0-mac.zip";
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("upload-intent-settle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let model = release_durable_post_intent_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // One upload POST that finished as `out`, settled by a cut of `kind`: the verdict, the
    // journal read back from disk, and whether the resume may post `NAME` again.
    let settled = |kind: CutKind, out: &ledger::RunOut| {
        let mut ctx = context(&root, true);
        ctx.kind = kind;
        // The journal's invariant: an upload intent implies the bound release.
        drop(ctx.persist_release_intent().unwrap());
        ctx.release_id = Some(55);
        let journal = ctx.journal.as_mut().unwrap();
        journal.release_id = Some(55);
        journal.save(&ctx.journal_path).unwrap();
        drop(ctx.persist_upload_intent(NAME).unwrap());
        drop(ctx.persist_upload_intent(OTHER).unwrap());
        let verdict = ctx.settle_upload_intent(NAME, out);
        let on_disk = Journal::load(&ctx.journal_path).unwrap().unwrap();
        assert_eq!(on_disk.upload_intents, ctx.upload_intents);
        let issued = ctx.upload_intents.iter().any(|issued| issued == NAME);
        let reposts = match publish::durable_post_decision(issued, false) {
            DurablePostDecision::PersistIntentThenPost => {
                drop(
                    ctx.persist_upload_intent(NAME)
                        .expect("a permit for the repost"),
                );
                true
            }
            DurablePostDecision::AwaitVisibility => {
                assert!(ctx.persist_upload_intent(NAME).is_err(), "no second permit");
                false
            }
            DurablePostDecision::ConvergeVisible => unreachable!("nothing is visible"),
        };
        // What the journal held when the settle returned, not after the repost above.
        (verdict, on_disk.upload_intents, reposts)
    };
    // The model when the upload POST has finished: `created_nothing` is the world (refused or
    // never left), which the process knows only then.
    let posted = |created_nothing: bool| {
        let mut state = model.init_state();
        for action in [
            "PersistCreateIntent",
            "IssueCreatePost",
            "RevealCreatedDraft",
            "ConvergeCreatedDraft",
            "PersistUploadIntent",
            if created_nothing {
                "IssueRefusedUploadPost"
            } else {
                "IssueUploadPost"
            },
        ] {
            step(&model, &mut state, action, action);
        }
        state
    };
    let out = |status: i32, stderr: &str| ledger::RunOut {
        status,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    };
    let http = |code: u16| {
        out(
            22,
            &format!("curl: (22) The requested URL returned error: {code}"),
        )
    };

    // Freed: only this POST's intent goes (the `--abandon` list still names every other
    // upload), and the resume may post it again.
    for (label, finished, verdict) in [
        (
            "curl refused its own arguments",
            out(2, "curl: option --upload-file: out of memory"),
            NeverSent,
        ),
        ("401: the token expired mid-cut", http(401), Refused),
        ("403: throttled", http(403), Refused),
        ("422: the upload was refused", http(422), Refused),
    ] {
        let (got, intents, reposts) = settled(CutKind::DryRun, &finished);
        assert_eq!(got.expect("a retraction"), verdict, "{label}");
        assert_eq!(intents, [OTHER], "{label}");
        assert!(reposts, "{label}");

        // The model: retractable, and once retracted a fresh intent may persist.
        let mut state = posted(true);
        assert!(
            model.action_enabled("RetractUploadIntent", &state),
            "{label}"
        );
        step(&model, &mut state, "RetractUploadIntent", label);
        assert_eq!(
            model.action_enabled("PersistUploadIntent", &state),
            reposts,
            "{label}"
        );
    }

    // Negative controls: each can hide a delivered request, so it keeps its intent (the resume
    // waits for visibility) and is never misread as a refusal.
    for (label, finished) in [
        ("502: the edge can hide a stored object", http(502)),
        ("500", http(500)),
        ("a timeout", out(28, "curl: (28) Operation timed out")),
        (
            "a reset",
            out(56, "curl: (56) Recv failure: Connection reset by peer"),
        ),
        ("killed", out(-1, "")),
        (
            "wording this code has never seen",
            out(22, "curl: (22) some future wording"),
        ),
        ("success: the probe decides", out(0, "")),
    ] {
        let (got, intents, reposts) = settled(CutKind::DryRun, &finished);
        assert_eq!(got.expect("no retraction to fail"), Unknown, "{label}");
        assert_eq!(intents, [NAME, OTHER], "{label}");
        assert!(!reposts, "{label}");

        // The model refuses to retract a POST that may have landed, and a fresh intent while
        // the old one stands; the mutant retracts anyway and leaves an object no intent names.
        let state = posted(false);
        assert!(
            !model.action_enabled("RetractUploadIntent", &state),
            "{label}"
        );
        assert_eq!(
            model.action_enabled("PersistUploadIntent", &state),
            reposts,
            "{label}"
        );
        let hidden = buggy.successors("RetractUploadIntent", &state)[0].clone();
        assert!(
            !buggy.check_invariant("LandedUploadStaysNamed", &hidden),
            "{label}: a blanket retraction hides a stored object"
        );
    }

    // An answered 4xx re-proves the lease; the never-sent exit does not. This real cut holds no
    // lease, like a process fenced out of its claim: a refusal it cannot prove it owns keeps its
    // intent and says why, while curl's exit 2 is retracted with no further call that can fail.
    let (got, intents, reposts) = settled(CutKind::Real, &http(422));
    let why = got.expect_err("a refusal from a process that cannot prove its lease");
    assert!(
        why.to_string().contains("KEPT") && why.to_string().contains("re-prove"),
        "{why}"
    );
    assert_eq!(
        intents,
        [NAME, OTHER],
        "the intent survives the failed proof"
    );
    assert!(!reposts);
    let (got, intents, reposts) = settled(
        CutKind::Real,
        &out(2, "curl: option --upload-file: out of memory"),
    );
    assert_eq!(got.expect("never sent needs no lease"), NeverSent);
    assert_eq!(intents, [OTHER]);
    assert!(reposts);

    // Mutant: a resumed process retracting on a refusal that died with the process that saw it.
    let mut crashed = buggy.init_state();
    for action in [
        "PersistCreateIntent",
        "IssueCreatePost",
        "RevealCreatedDraft",
        "ConvergeCreatedDraft",
        "PersistUploadIntent",
        "IssueRefusedUploadPost",
        "Crash",
    ] {
        assert!(buggy.fire(action, &mut crashed), "{action}");
    }
    assert!(!buggy.check_invariant("UploadRefusalIsProcessLocal", &crashed));
    let mut healthy = posted(true);
    step(
        &model,
        &mut healthy,
        "Crash",
        "the refusal dies with its process",
    );
    step(&model, &mut healthy, "Resume", "the resume never saw it");
    assert!(
        !model.action_enabled("RetractUploadIntent", &healthy),
        "a resumed process waits for the object, as the resume does"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// What the upload does after its POST is one pure decision (`publish::after_upload_post`),
/// shared by `upload_channel_asset` and the fake GitHub. The probe is never asked when the POST
/// provably never left, best-effort after a refusal (`422 already_exists` is a 4xx too, so an
/// object there is converged on; a failed probe leaves the refusal standing), and decisive after
/// an unknown outcome.
#[test]
fn what_an_upload_does_after_its_post_is_one_table() {
    use publish::AfterUploadPost::{Ambiguous, Converge, NeverSent, Refused};
    use publish::PostOutcome;
    let decide = |outcome, seen: Result<bool, ()>| {
        let settled = std::cell::Cell::new(false);
        let mut asked = 0;
        let verdict = publish::after_upload_post(
            || {
                settled.set(true);
                Ok(outcome)
            },
            || {
                assert!(settled.get(), "the intent is settled before any probe");
                asked += 1;
                seen.map_err(|()| ledger::Error::new("the probe failed"))
            },
        );
        (verdict.map_err(|error| error.to_string()), asked)
    };
    // A settle that fails asks nothing of the network.
    let unsettled = publish::after_upload_post(
        || Err(ledger::Error::new("the journal could not be written")),
        || -> ledger::Result<bool> { panic!("a probe before the settle") },
    );
    assert!(unsettled.is_err());

    // Never sent: local and provable, so the probe is never asked.
    assert_eq!(decide(PostOutcome::NeverSent, Ok(true)), (Ok(NeverSent), 0));
    assert_eq!(decide(PostOutcome::NeverSent, Err(())), (Ok(NeverSent), 0));

    // Refused: one best-effort probe. Visible converges and asks the journal to name it;
    // absent, or a failed probe, is the refusal as reported.
    assert_eq!(
        decide(PostOutcome::Refused, Ok(true)),
        (Ok(Converge { journal: true }), 1)
    );
    assert_eq!(decide(PostOutcome::Refused, Ok(false)), (Ok(Refused), 1));
    assert_eq!(decide(PostOutcome::Refused, Err(())), (Ok(Refused), 1));

    // Unknown: the probe decides; the intent was never retracted, so a visible object is
    // already named. A failed probe fails the step and the intent stands.
    assert_eq!(
        decide(PostOutcome::Unknown, Ok(true)),
        (Ok(Converge { journal: false }), 1)
    );
    assert_eq!(decide(PostOutcome::Unknown, Ok(false)), (Ok(Ambiguous), 1));
    let (failed, asked) = decide(PostOutcome::Unknown, Err(()));
    assert!(failed.is_err() && asked == 1, "{failed:?}");
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
    // The operator answers for a lost journal only; a journal proving a POST still wins.
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
    // `bound`: the visible draft targets the recovery claim's commit, so the remote proves it
    // is this claim's. Only that stands in for a lost journal, and only with a visible draft.
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

/// Project one recovery observation onto `ReleaseHistoricalRecovery`: what the journal knows
/// (`None` = lost), and whether a draft is visible and bound to the claim's commit.
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

/// The delete authority for one row: `draft_cleanup_decision` says delete and the draft passes
/// `validate_release_object_capability` against the claim's commit.
/// `publish::withdraw_unpublished_release` makes the same decision but binds the draft by its
/// asset names (`channel::binds_to_version`), which this does not pin.
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

/// The healthy model's delete authority at a projected state: `DeleteExactDraft` now, or once
/// the remote's claim binding has answered a lost journal.
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

/// Tier-1 for `ReleaseHistoricalRecovery`: every row of the cleanup selector
/// (`draft_cleanup_decision` with the delete's capability check, and `absent_draft_decision`
/// with and without `--no-draft-was-posted`) is projected onto the model, whose healthy actions
/// admit exactly what the code does; the mutants are replayed on the rows the code refuses.
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
                // A visible draft is never abandoned: only the absent-listing lane abandons.
                assert!(!model.action_enabled("AbandonProvenNoPost", &state));
                assert!(!model.action_enabled("AbandonOnOperatorNoPostAnswer", &state));
            }
            // Negative controls on the rows the shipping code refuses to delete.
            if visible && !bound {
                assert!(!real);
                if knowledge.is_none() {
                    // The lost-journal row with its `claim_bound` conjunct dropped.
                    let deleted = buggy.successors("DeleteUnknownDraft", &state)[0].clone();
                    assert!(!buggy.check_invariant("DraftDeletionRequiresIssuedIntent", &deleted));
                    unbound_lost_journal = true;
                }
                if knowledge == Some(true) {
                    // `draft_cleanup_decision` alone says delete; only the capability check
                    // refuses, and skipping it deletes someone else's draft.
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

    // The absent listing abandons on a journal proving no POST, or on the operator's answer for
    // a lost journal, never over a journal proving one.
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
            // Mutant: the answer folded into `--old-publisher-stopped`, unlocking with no one
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

    // The owner's lease is released only after the tag cleanup: the recovery and
    // `--abandon` both end through `publish::withdraw_tag_then_unlock`, whose one body
    // runs them in that order; the model's own transitions are held to it here.
    let mut cleaned = recovery_state(&model, Some(true), true, true);
    assert!(model.fire("DeleteExactDraft", &mut cleaned));
    assert!(!model.action_enabled("ReleaseOwnerBeforeTagCleanup", &cleaned));
    let early = buggy.successors("ReleaseOwnerBeforeTagCleanup", &cleaned)[0].clone();
    assert!(!buggy.check_invariant("CompletionReleasesOwner", &early));
}

#[test]
fn curl_transport_preflight_requires_every_no_retry_post_option() {
    // `--upload-file` is required because the asset leg streams: a batteries-included DMG is
    // over a gigabyte and `--data-binary @file` buffers it whole. A curl that cannot stream
    // fails the preflight, not the upload.
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
fn strict_draft_delete_never_converges_on_pre_delete_absence() {
    assert!(!publish::exact_delete_absence_is_converged(false, false));
    assert!(publish::exact_delete_absence_is_converged(false, true));
    assert!(publish::exact_delete_absence_is_converged(true, false));
    assert!(publish::exact_delete_absence_is_converged(true, true));
}
