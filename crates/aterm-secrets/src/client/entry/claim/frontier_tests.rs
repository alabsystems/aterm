// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

fn named(paths: &Paths, name: &str) -> EntrySnapshot {
    read_verified_entry(paths, &config(paths).unwrap().unwrap(), name)
        .unwrap()
        .unwrap()
        .snapshot
}

/// The candidate is written after one of two independently signed heads. A
/// merged read sees both heads in either case; only the second candidate was
/// authored after both. Merely finding the cause's operation is insufficient.
fn split_frontier(after_both: bool) -> bool {
    use crate::commons::bootstrap::Bootstrap;
    use crate::membership_log::EpochAudience;
    let _answering = ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs(&[
        (Audience::Everyone, "token", b"initial"),
        (Audience::Everyone, "other", b"cause"),
    ]);
    let base = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let identity = read_identity(&lab.m22).unwrap().unwrap();
    let opened = base.open_audience_entries(&identity).unwrap().unwrap();
    let epoch = opened.maximal_epochs[&EpochAudience::Everyone][0];
    let key = *opened.unwrapped_keys[&epoch];
    let draft = |history: &crate::membership_log::Log, paths: &Paths, name: &str| {
        let bootstrap = Bootstrap::required(paths).unwrap();
        let request = bootstrap.request().unwrap();
        bootstrap.device().with_seed(|seed| {
            history
                .draft_put(
                    &request.binding().actor(),
                    2_000,
                    epoch,
                    &key,
                    &Entry {
                        name: name.into(),
                        value: SecretBytes::from(b"new value".as_slice()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(seed, *request.binding().incarnation())
                .unwrap()
        })
    };
    let first = draft(&base, &lab.m22, "other");
    let second = draft(&base, &lab.m7, "unrelated");
    let mut branch = base.clone();
    branch.insert(first.clone()).unwrap();
    let mut merged = branch.clone();
    merged.insert(second).unwrap();
    let install = |history: &crate::membership_log::Log| {
        let wire = crate::membership_log::encode_records(history.records());
        save_records(&lab.m22, &wire).unwrap();
        transport::post_records(&lab.url, &wire).unwrap();
        sync(&lab.m22, None).unwrap();
    };
    install(&merged);
    let cause = named(&lab.m22, "other").entry_claim().unwrap().unwrap();
    assert_eq!(cause.version.log().unwrap().heads.len(), 2);
    let candidate = draft(
        if after_both { &merged } else { &branch },
        &lab.m22,
        "token",
    );
    merged.insert(candidate.clone()).unwrap();
    install(&merged);
    let current = named(&lab.m22, "token");
    assert_eq!(current.semantic_operation(), Some(*candidate.id()));
    let result = current.follows_claim(&lab.m22, &cause).unwrap();
    lab.server.stop();
    result
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    if action == "Case" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Read");
    let scenario = state["scenario"];
    let f = tests::fixture();
    let mut cause = named(&f.paths, "other").entry_claim().unwrap().unwrap();
    let initial = tests::snapshot(&f);
    let mut current;
    let mut direct = None;
    match scenario {
        0 => {
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        1 => current = initial,
        2 => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"unrelated change")
                    .unwrap();
            });
            current = tests::snapshot(&f);
        }
        3 => {
            admin_revoke(&f.paths, &admin_open(&f.paths).unwrap(), "peer", false).unwrap();
            current = tests::snapshot(&f);
            assert_eq!(current.semantic_operation(), initial.semantic_operation());
            assert_ne!(current.wrapper_operation(), initial.wrapper_operation());
        }
        4 => {
            let mut log = cause.version.log().unwrap().clone();
            log.heads = vec![[42; 32]];
            cause.version = VersionClaim::new(
                cause.version.vault().into(),
                *cause.version.signer(),
                cause.version.envelope().cloned(),
                Some(log),
            )
            .unwrap();
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        5 => {
            cause.operation = [42; 32];
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        6 => {
            cause.recipient = Identity::from_secret_bytes([43; 32]).recipient();
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        7 => {
            cause.host = "another-host".into();
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        8 => {
            cause.audience = Audience::Hosts(vec!["admin".into()]);
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        9 => {
            tests::update(&f, b"changed");
            tests::envelope_fallback(&f);
            current = tests::snapshot(&f);
            assert_eq!(current.origin(), super::super::EntryOrigin::Envelope);
        }
        10 | 11 => {
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
            let Source::Log {
                register: Some(register),
                ..
            } = &mut current.source
            else {
                panic!("missing selected log register")
            };
            if scenario == 10 {
                register.ambiguous = true;
            } else {
                register.complete = false;
            }
        }
        12 => {
            cause = initial.entry_claim().unwrap().unwrap();
            current = initial;
        }
        13 | 14 => {
            direct = Some(split_frontier(scenario == 13));
            current = initial;
        }
        15 => {
            tests::update(&f, b"changed before retirement");
            cause = named(&f.paths, "other").entry_claim().unwrap().unwrap();
            current = tests::snapshot(&f);
        }
        16 => {
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
            PREFIX_ERROR.set(true);
        }
        17 => {
            tests::update(&f, b"middle");
            tests::update(&f, b"inert-authentic-token");
            assert!(!initial.follows_claim(&f.paths, &cause).unwrap());
            let admitted = initial
                .admit(&f.paths, |entry| {
                    entry.snapshot().follows_claim(&f.paths, &cause)
                })
                .unwrap();
            let super::super::EntryAdmission::Accepted(proved) = admitted else {
                panic!("fresh entry admission refused")
            };
            direct = Some(proved.unwrap());
            current = initial;
        }
        18 => {
            direct = Some(tests::historical_roster_denial_checks().1);
            current = initial;
        }
        19..=21 => {
            let mut log = cause.version.log().unwrap().clone();
            if scenario == 21 {
                log.network = [43; 32];
            }
            cause.version = VersionClaim::new(
                if scenario == 19 {
                    "another-vault".into()
                } else {
                    cause.version.vault().into()
                },
                if scenario == 20 {
                    [43; 32]
                } else {
                    *cause.version.signer()
                },
                cause.version.envelope().cloned(),
                Some(log),
            )
            .unwrap();
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        22 => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"future cause")
                    .unwrap();
            });
            cause = named(&f.paths, "other").entry_claim().unwrap().unwrap();
            current = initial;
        }
        23 => {
            cause = initial.entry_claim().unwrap().unwrap();
            tests::update(&f, b"changed");
            current = tests::snapshot(&f);
        }
        24 => {
            tests::update(&f, b"middle");
            tests::update(&f, b"inert-authentic-token");
            current = tests::snapshot(&f);
        }
        _ => panic!("unknown frontier scenario"),
    }
    let proved = match direct {
        Some(proved) => proved,
        None => current.follows_claim(&f.paths, &cause).unwrap(),
    };
    PREFIX_ERROR.set(false);
    next.insert("done", 1);
    next.insert("proved", i64::from(proved));
    next
}

#[test]
fn entry_frontier_requires_the_entire_authenticated_retirement_cut() {
    let model = aterm_models_db::entry_frontier_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
            }
        }
    }
    for (mutant, scenario, invariant) in [
        (1, 5, "AuthenticatedCut"),
        (2, 3, "SemanticWrite"),
        (3, 14, "WholeFrontier"),
        (4, 11, "CompleteSelection"),
        (5, 15, "StrictlyAfterCut"),
        (6, 0, "CausalProgressRecognized"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        FOLLOWS_MUTANT.set(mutant);
        let bad = transition(&state, "Read");
        FOLLOWS_MUTANT.set(0);
        assert!(
            !model.check_invariant(invariant, &bad),
            "mutant {mutant} survived"
        );
    }
}
