// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::entry::admission::tests::writer_is_free;
use crate::client::entry::tests::Fixture;
use aterm_spec::interp::State as ModelState;

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        MUTANT.set(0);
        VIEW_MUTANT.set(0);
        view::SKIP_STATE.set(false);
        view::SKIP_ROSTER.set(false);
    }
}

fn empty() -> Fixture {
    let fixture = Fixture::new();
    fixture.publish(|doc| {
        assert!(doc.unset_entry(&Audience::Everyone, "token").unwrap());
        assert!(doc.unset_entry(&Audience::Everyone, "other").unwrap());
    });
    assert!(fixture.read_token().is_none());
    fixture
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    transition_with(state, action, 0)
}

fn transition_with(state: &ModelState, action: &str, mutant: u8) -> ModelState {
    let mut next = state.clone();
    if action == "Case" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Read");
    let fixture = empty();
    let mut expected = config(&fixture.paths).unwrap().unwrap();
    let scenario = state["scenario"];
    let foreign = (scenario == 4).then(Fixture::new);
    match scenario {
        1 | 2 => {
            fixture.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "pick", b"inert-pick")
                    .unwrap();
            });
            if scenario == 2 {
                std::fs::remove_file(
                    materialize::target_path(&fixture.paths.home, "pick").unwrap(),
                )
                .unwrap();
            }
        }
        6 => std::fs::remove_file(fixture.paths.source_file()).unwrap(),
        7 | 10 => {
            let mut source = load_state(&fixture.paths).unwrap();
            if scenario == 7 {
                source.failed = true;
            } else {
                source.membership = Stored::Revoked;
            }
            save_state(&fixture.paths, &source).unwrap();
        }
        8 => expected.signer = None,
        9 => expected.host = "different-host".into(),
        11 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "unrelated", b"inert-unrelated")
                .unwrap();
        }),
        12 => fixture.publish(|doc| {
            let roster = crate::swarm_roster::SwarmRoster {
                deny_labels: vec!["m22".into()],
                members: vec![crate::swarm_roster::SwarmMember {
                    recipient: read_identity(&fixture.paths)
                        .unwrap()
                        .unwrap()
                        .recipient()
                        .to_bech32(),
                    vault_name: expected.host.clone(),
                    label: "m22".into(),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: None,
                    node_key: None,
                }],
                ..Default::default()
            };
            let encoded = roster.to_toml();
            let parsed = crate::swarm_roster::SwarmRoster::from_toml(&encoded).unwrap();
            assert!(parsed.is_member_denied(&parsed.members[0]));
            doc.set_entry(
                &Audience::Everyone,
                crate::SWARM_ROSTER_ENTRY,
                encoded.as_bytes(),
            )
            .unwrap();
        }),
        _ => {}
    }
    let writer = (scenario == 3).then(|| StateLock::take(&fixture.paths).unwrap());
    let look_paths = foreign.as_ref().map_or(&fixture.paths, |f| &f.paths);
    let look = VaultLook::new(look_paths);
    drop(writer);
    if scenario == 5 {
        assert!(look.held_at(&fixture.paths).is_some());
        std::fs::rename(
            fixture.paths.lock_file(),
            fixture._root.path().join("old-lock"),
        )
        .unwrap();
        fsx::write_atomic(&fixture.paths.lock_file(), b"", 0o600).unwrap();
    }
    let before = fixture.bytes();
    let observed = crate::client::STATE_OBSERVATIONS.get();
    let marker = fixture._root.path().join("local-intent");
    let (mut entered, mut held, mut candidate) = (false, false, 0);
    let _restore = Restore;
    match mutant {
        1 => VIEW_MUTANT.set(1),
        2 => VIEW_MUTANT.set(2),
        3 => view::SKIP_STATE.set(true),
        4 => MUTANT.set(2),
        5 => view::SKIP_ROSTER.set(true),
        _ => {}
    }
    let result = look
        .with_entry_reader(&fixture.paths, &expected, |reader| {
            entered = true;
            held = !writer_is_free(&fixture.paths);
            candidate = match reader.observe("pick").unwrap() {
                EntryRead::Present(value) => {
                    assert_eq!(value.value().as_slice(), b"inert-pick");
                    1
                }
                EntryRead::Absent(_) => 2,
                EntryRead::Unavailable => 3,
            };
            let second_absent = matches!(reader.observe("next").unwrap(), EntryRead::Absent(_));
            if candidate == 2 && second_absent {
                fsx::write_atomic(&marker, b"prepared intent", 0o600).unwrap();
            }
        })
        .unwrap();
    let acquired = crate::client::STATE_OBSERVATIONS.get() != observed;
    let after = !writer_is_free(&fixture.paths);
    drop(look);
    let free = writer_is_free(&fixture.paths);
    next.insert("done", 1);
    next.insert(
        "outcome",
        match result {
            EntryAdmission::Accepted(()) => 1,
            EntryAdmission::Unavailable => 3,
            _ => panic!("unexpected admission outcome"),
        },
    );
    next.insert("entered", i64::from(entered));
    next.insert("candidate", candidate);
    next.insert("intent", i64::from(marker.exists()));
    next.insert("held", i64::from(held));
    next.insert("after", i64::from(after));
    next.insert("free", i64::from(free));
    next.insert("kept", i64::from(fixture.bytes() == before));
    next.insert("acquired", i64::from(acquired));
    next
}

#[test]
fn entry_view_refines_authenticated_absence_and_retained_custody() {
    let model = aterm_models_db::entry_view_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 13);
    for (scenario, mutant, law) in [
        (5, 1, "CallbackOnlyAuthenticated"),
        (3, 2, "NoReacquisition"),
        (7, 3, "AuthenticatedOnly"),
        (2, 4, "NoFallbackFromUnavailable"),
        (12, 5, "AuthenticatedOnly"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        let bad = transition_with(&state, "Read", mutant);
        assert!(!model.check_invariant(law, &bad), "mutant {mutant}");
    }
}

#[test]
fn view_callback_failure_and_unwind_keep_custody_with_the_owner() {
    let fixture = empty();
    let expected = config(&fixture.paths).unwrap().unwrap();
    let look = VaultLook::new(&fixture.paths);
    let before = fixture.bytes();
    let result = look
        .with_entry_reader(&fixture.paths, &expected, |_| Err::<(), _>("refused"))
        .unwrap();
    assert!(matches!(result, EntryAdmission::Accepted(Err("refused"))));
    assert!(!writer_is_free(&fixture.paths));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = look.with_entry_reader(&fixture.paths, &expected, |_| panic!("unwind"));
    }));
    assert!(result.is_err());
    assert!(!writer_is_free(&fixture.paths));
    drop(look);
    assert!(writer_is_free(&fixture.paths));
    assert_eq!(fixture.bytes(), before);
}

#[test]
fn view_authentication_failure_never_enters_the_callback() {
    let fixture = empty();
    let expected = config(&fixture.paths).unwrap().unwrap();
    let unconfigured = Paths::for_home(&fixture._root.path().join("unconfigured"));
    let absent = VaultLook::new(&unconfigured);
    assert!(matches!(
        absent
            .with_entry_reader(&unconfigured, &expected, |_| panic!(
                "unconfigured view admitted"
            ))
            .unwrap(),
        EntryAdmission::Unavailable
    ));
    assert!(!unconfigured.dir.exists());
    let look = VaultLook::new(&fixture.paths);
    std::fs::write(fixture.paths.cache_file(), b"forged cache").unwrap();
    let result = look.with_entry_reader(&fixture.paths, &expected, |_| {
        panic!("forged view admitted")
    });
    assert!(matches!(result, Err(_) | Ok(EntryAdmission::Unavailable)));
    assert!(!writer_is_free(&fixture.paths));
    drop(look);
    assert!(writer_is_free(&fixture.paths));
}
