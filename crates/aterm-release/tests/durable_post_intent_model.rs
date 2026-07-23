// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for journal-v5 one-shot GitHub POST authority.

#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use aterm_spec::derive::{
    Model, release_durable_post_intent_model, release_historical_recovery_model,
};
use publish::{CutCtx, CutKind, DurablePostDecision, Journal};
use std::path::{Path, PathBuf};
use std::process::Command;

fn journal() -> Journal {
    Journal {
        format: publish::JOURNAL_FORMAT,
        version: "0.55".into(),
        build_number: 55,
        commit: "a".repeat(40),
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        done: Vec::new(),
    }
}

fn context(root: &Path, with_journal: bool) -> CutCtx {
    let journal_path = root.join("nested/dist/cut-state.toml");
    CutCtx {
        repo: root.to_path_buf(),
        dist: root.join("nested/dist"),
        journal_path,
        slug: "owner/repo".into(),
        version: "0.55".into(),
        tag: "v0.55".into(),
        build: 55,
        commit: "a".repeat(40),
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        kind: CutKind::Real,
        lease: None,
        fence: None,
        notes_section: "0.55".into(),
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
    let upload_permit = ctx.persist_upload_intent("aterm-0.55.dmg").unwrap();
    drop(upload_permit);
    let uploaded = Journal::load(&ctx.journal_path).unwrap().unwrap();
    assert_eq!(uploaded.upload_intents, ["aterm-0.55.dmg"]);
    assert!(ctx.persist_upload_intent("aterm-0.55.dmg").is_err());

    // A real publication context without a journal cannot mint authority.
    let mut unjournaled = context(&root, false);
    assert!(unjournaled.persist_draft_create_intent().is_err());
    assert!(unjournaled.persist_upload_intent("aterm-0.55.dmg").is_err());

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
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
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
        publish::absent_draft_decision(Some(false)),
        publish::AbsentDraftDecision::AbandonProvenNoPost
    );
    for knowledge in [Some(true), None] {
        assert_eq!(
            publish::absent_draft_decision(knowledge),
            publish::AbsentDraftDecision::RetainOwnerAwaitVisibility
        );
    }
    use publish::DraftCleanupDecision::{
        AbandonProvenNoPost, DeleteIssuedVisible, RefuseUnknownOrInconsistent,
        RetainIssuedAwaitVisibility,
    };
    for (knowledge, visible, expected) in [
        (Some(false), false, AbandonProvenNoPost),
        (Some(false), true, RefuseUnknownOrInconsistent),
        (Some(true), false, RetainIssuedAwaitVisibility),
        (Some(true), true, DeleteIssuedVisible),
        (None, false, RefuseUnknownOrInconsistent),
        (None, true, RefuseUnknownOrInconsistent),
    ] {
        assert_eq!(
            publish::draft_cleanup_decision(knowledge, visible),
            expected,
            "cleanup matrix drift for {knowledge:?}, visible={visible}"
        );
    }

    let cleanup_model = release_historical_recovery_model();
    let unknown = cleanup_model.init_state();
    assert!(!cleanup_model.action_enabled("AbandonProvenNoPost", &unknown));
    assert!(!cleanup_model.action_enabled("DeleteExactDraft", &unknown));
    let mut known_none_visible = cleanup_model.init_state();
    assert!(cleanup_model.fire("LearnNoPostFromCurrentJournal", &mut known_none_visible));
    assert!(cleanup_model.fire("ObserveExactDraft", &mut known_none_visible));
    assert!(!cleanup_model.action_enabled("AbandonProvenNoPost", &known_none_visible));
    assert!(!cleanup_model.action_enabled("DeleteExactDraft", &known_none_visible));
    let mut issued_visible = cleanup_model.init_state();
    assert!(cleanup_model.fire("LearnIssuedIntentFromCurrentJournal", &mut issued_visible));
    assert!(cleanup_model.fire("ObserveExactDraft", &mut issued_visible));
    assert!(cleanup_model.fire("DeleteExactDraft", &mut issued_visible));
    assert!(cleanup_model.fire("AbandonDeletedIssuedDraft", &mut issued_visible));
    assert_eq!(issued_visible["owner_held"], 0);

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

#[test]
fn curl_transport_preflight_requires_every_no_retry_post_option() {
    let help = "--data-binary --fail-with-body --header --request --retry \
                --show-error --silent --url";
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

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn exact_cask_commit_proof_rejects_an_extra_path() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("exact-cask-commit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("packaging/homebrew")).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    git(&root, &["config", "user.name", "Cask Proof"]);
    git(
        &root,
        &["config", "user.email", "cask-proof@example.invalid"],
    );
    let cask = "packaging/homebrew/aterm.rb";
    std::fs::write(root.join(cask), "version \"0.54\"\n").unwrap();
    std::fs::write(root.join("README"), "base\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "base"]);
    let base = git(&root, &["rev-parse", "HEAD"]);

    let expected = "version \"0.55\"\n";
    let message = "release: v0.55 cask pin";
    std::fs::write(root.join(cask), expected).unwrap();
    git(&root, &["add", "--", cask]);
    git(&root, &["commit", "-m", message, "--only", "--", cask]);
    let exact = git(&root, &["rev-parse", "HEAD"]);
    let cli = ledger::GitCli::new(&root);
    publish::prove_exact_cask_commit(&cli, &exact, &base, cask, expected, message).unwrap();

    std::fs::write(root.join(cask), "version \"0.56\"\n").unwrap();
    std::fs::write(root.join("README"), "mutated\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", message]);
    let extra = git(&root, &["rev-parse", "HEAD"]);
    assert!(
        publish::prove_exact_cask_commit(&cli, &extra, &exact, cask, "version \"0.56\"\n", message)
            .is_err()
    );
    let _ = std::fs::remove_dir_all(root);
}
