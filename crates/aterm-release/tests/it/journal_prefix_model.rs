// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the release journal's exact-prefix authority.
//! Every one of the 2^6 done-membership subsets of the publish-once step list is
//! checked against production; only the 7 canonical prefixes may persist or reload.

use crate::publish;

use aterm_spec::derive::{Model, release_journal_prefix_model};

fn journal(done: Vec<String>) -> publish::Journal {
    let release_id = done.iter().any(|step| step == "publish").then_some(55);
    publish::Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.55.0".into(),
        build_number: 55,
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        min_build: Some(55),
        arm64_only: false,
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id,
        release_intent: release_id.is_some(),
        upload_intents: Vec::new(),
        done,
    }
}

fn model_step(
    model: &Model,
    before: &aterm_spec::interp::State,
    action: &str,
    label: &str,
) -> aterm_spec::interp::State {
    assert!(model.action_enabled(action, before), "disabled {action}");
    let after = model.successors(action, before)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[],
        before,
        &after,
        Some(action),
        label,
    );
    assert!(admitted, "model rejected production decision: {why}");
    after
}

fn abstract_prefix(model: &Model, count: usize) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    for action in ["InputLock", "InputPrepare", "InputVisible", "InputUnlock"]
        .into_iter()
        .take(count)
    {
        assert!(model.fire(action, &mut state));
    }
    state
}

#[test]
fn every_production_done_subset_matches_exact_prefix_model() {
    let model = release_journal_prefix_model();
    let root =
        std::env::temp_dir().join(format!("aterm-journal-prefix-tier1-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("cut-state.toml");
    let mut admitted_masks = Vec::new();

    for mask in 0_usize..(1_usize << publish::STEPS.len()) {
        let done = publish::STEPS
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1_usize << index) != 0)
            .map(|(_, step)| (*step).to_string())
            .collect::<Vec<_>>();
        let real = journal(done).save(&path);
        let prefix_len = mask.count_ones() as usize;
        let canonical_mask = if prefix_len == 0 {
            0
        } else {
            (1_usize << prefix_len) - 1
        };
        let expected = mask == canonical_mask;
        assert_eq!(
            real.is_ok(),
            expected,
            "production journal verdict drifted for done mask {mask:06b}"
        );
        if expected {
            admitted_masks.push(mask);
            assert_eq!(
                publish::Journal::load(&path).unwrap(),
                Some(journal(
                    publish::STEPS[..prefix_len]
                        .iter()
                        .map(|step| (*step).to_string())
                        .collect()
                ))
            );
        }
    }
    assert_eq!(
        admitted_masks.len(),
        publish::STEPS.len() + 1,
        "only empty plus each canonical prefix is admissible"
    );

    // Bind the four model boundaries to concrete pipeline prefixes: empty,
    // lock, prepared (build, self-check and the origin tag), published (the one
    // publication — visible-channel convergence), and unlock-complete.
    assert_eq!(
        publish::STEPS,
        ["lock", "build", "selfcheck", "tag", "publish", "unlock"]
    );
    for (abstract_count, concrete_count, action) in [
        (0, 0, "AdmitEmptyPrefix"),
        (1, 1, "AdmitLockPrefix"),
        (2, 4, "AdmitPreparePrefix"),
        (3, 5, "AdmitVisiblePrefix"),
        (4, 6, "AdmitCompletePrefix"),
    ] {
        let done = publish::STEPS[..concrete_count]
            .iter()
            .map(|step| (*step).to_string())
            .collect();
        assert!(journal(done).save(&path).is_ok());
        let before = abstract_prefix(&model, abstract_count);
        let after = model_step(
            &model,
            &before,
            action,
            "release journal: exact production prefix admission",
        );
        assert_eq!(after["resume_cursor"], abstract_count as i64);
    }

    // Unknown/duplicate names and malformed immutable identity fail before any
    // resume authority is admitted.
    for done in [
        vec!["lock".into(), "unknown".into()],
        vec!["lock".into(), "lock".into()],
        vec!["lock".into(), "build".into(), "unlock".into()],
    ] {
        assert!(journal(done).save(&path).is_err());
    }
    // The journal carries the canonical MAJOR.MINOR.PATCH release version, so
    // the retired two-component spelling, an over-long one, and a non-canonical
    // leading-zero component are all refused before any resume authority.
    for bad in ["0.55", "0.55.1.2", "0.55.01", "0.55.x", ""] {
        let mut bad_version = journal(Vec::new());
        bad_version.version = bad.into();
        assert!(
            bad_version.save(&path).is_err(),
            "journal accepted non-canonical version {bad:?}"
        );
    }
    let mut bad_owner = journal(Vec::new());
    bad_owner.commit = "abcd".into();
    assert!(bad_owner.save(&path).is_err());

    // NEGATIVE CONTROL: the historical gap (later remote mutation pre-marked)
    // is rejected by production and by Buggy=0, but admitted by Buggy=1.
    let dangerous_gap = vec!["lock".into(), "publish".into()];
    assert!(journal(dangerous_gap).save(&path).is_err());
    let mut before = model.init_state();
    assert!(model.fire("InputLock", &mut before));
    assert!(model.fire("InputVisible", &mut before));
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let bypassed = buggy.successors("AdmitGappedJournal", &before)[0].clone();
    let (healthy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &bypassed,
        Some("AdmitGappedJournal"),
        "release journal: reject gapped remote-mutation prefix",
    );
    assert!(!healthy_admitted, "healthy model admitted gap: {why}");
    let (buggy_admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &before,
        &bypassed,
        Some("AdmitGappedJournal"),
        "release journal: gapped-prefix negative control",
    );
    assert!(buggy_admitted, "Buggy=1 did not admit gap: {why}");
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &bypassed));

    // NEGATIVE CONTROL: a journal through `mirror` is not complete — production
    // resumes it at `unlock`. Completing it anyway (the cut reported DONE over a
    // final step that never journaled) is the mutant only the completion law sees.
    let through_mirror = journal(
        publish::STEPS[..publish::STEPS.len() - 1]
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    );
    assert_eq!(through_mirror.first_incomplete(), Some("unlock"));
    let visible = model_step(
        &model,
        &abstract_prefix(&model, 3),
        "AdmitVisiblePrefix",
        "release journal: verify-complete prefix admitted",
    );
    assert!(!model.action_enabled("CompleteBeforeUnlockJournaled", &visible));
    let unfinished = buggy.successors("CompleteBeforeUnlockJournaled", &visible)[0].clone();
    assert!(!buggy.check_invariant("CompletionRequiresEveryStep", &unfinished));

    let _ = std::fs::remove_file(path);
}
