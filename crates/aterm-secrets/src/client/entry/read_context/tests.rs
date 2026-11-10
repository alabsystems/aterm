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
    }
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
    assert_eq!(action, "Admit");
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let scenario = state["scenario"];
    let foreign = (scenario == 3).then(Fixture::new);
    match scenario {
        1 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "other", b"new-other")
                .unwrap();
        }),
        2 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "token", b"new-token")
                .unwrap();
        }),
        6 => std::fs::remove_file(materialize::target_path(&fixture.paths.home, "other").unwrap())
            .unwrap(),
        _ => {}
    }
    let writer = (scenario == 4).then(|| StateLock::take(&fixture.paths).unwrap());
    let look_paths = foreign.as_ref().map_or(&fixture.paths, |f| &f.paths);
    let look = VaultLook::new(look_paths);
    drop(writer);
    if scenario == 5 {
        // Prime the look's memo, then replace its path without taking its lock.
        assert!(look.held_at(&fixture.paths).is_some());
        std::fs::rename(
            fixture.paths.lock_file(),
            fixture._root.path().join("old-lock"),
        )
        .unwrap();
        fsx::write_atomic(&fixture.paths.lock_file(), b"", 0o600).unwrap();
    }
    let before = fixture.bytes();
    let marker = fixture._root.path().join("local-intent");
    let (mut entered, mut held, mut candidate) = (false, false, 0);
    let _restore = Restore;
    MUTANT.set(mutant);
    let result = snapshot
        .admit_in(&fixture.paths, &look, |entry, reader| {
            entered = true;
            held = !writer_is_free(&fixture.paths);
            assert_eq!(entry.value().as_slice(), b"inert-authentic-token");
            let name = match scenario {
                7 => "not-listed",
                8 => crate::commons::ENTRY,
                _ => "other",
            };
            candidate = match reader.observe(name) {
                Ok(EntryRead::Present(other)) => {
                    assert_eq!(
                        other.value().as_slice(),
                        if scenario == 1 {
                            b"new-other".as_slice()
                        } else {
                            b"inert-other".as_slice()
                        }
                    );
                    1
                }
                Ok(EntryRead::Absent(_)) => 2,
                Ok(EntryRead::Unavailable) => 3,
                Err(_) => 4,
            };
            if matches!(candidate, 1 | 2) {
                fsx::write_atomic(&marker, b"prepared intent", 0o600).unwrap();
            }
        })
        .unwrap();
    MUTANT.set(0);
    let after = !writer_is_free(&fixture.paths);
    drop(look);
    let free = writer_is_free(&fixture.paths);
    next.insert("done", 1);
    next.insert(
        "outcome",
        match result {
            EntryAdmission::Accepted(()) => 1,
            EntryAdmission::Busy => 2,
            EntryAdmission::Unavailable => 3,
            EntryAdmission::Changed => 4,
        },
    );
    next.insert("entered", i64::from(entered));
    next.insert("held", i64::from(held));
    next.insert("candidate", candidate);
    next.insert("intent", i64::from(marker.exists()));
    next.insert("after", i64::from(after));
    next.insert("free", i64::from(free));
    next.insert("kept", i64::from(before == fixture.bytes()));
    next
}

#[test]
fn borrowed_entry_reads_refine_custody_and_preserve_unavailable_candidates() {
    let model = aterm_models_db::entry_read_context_model();
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
                cases += usize::from(action.name == "Admit");
            }
        }
    }
    assert_eq!(cases, 9);
    for (scenario, mutant, invariant) in [
        (5, 1, "CallbackOnlyCurrent"),
        (6, 2, "CandidateOutcome"),
        (4, 3, "CallbackOnlyCurrent"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        let bad = transition_with(&state, "Admit", mutant);
        assert!(!model.check_invariant(invariant, &bad), "mutant {mutant}");
    }
}

#[test]
fn callback_failure_and_unwind_leave_the_borrowed_guard_with_its_owner() {
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let before = fixture.bytes();
    let look = VaultLook::new(&fixture.paths);
    let result = snapshot
        .admit_in(&fixture.paths, &look, |_, _| Err::<(), _>("refused"))
        .unwrap();
    assert!(matches!(result, EntryAdmission::Accepted(Err("refused"))));
    assert!(!writer_is_free(&fixture.paths));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = snapshot.admit_in(&fixture.paths, &look, |_, _| panic!("unwind"));
    }));
    assert!(result.is_err());
    assert!(!writer_is_free(&fixture.paths));
    drop(look);
    assert!(writer_is_free(&fixture.paths));
    assert_eq!(fixture.bytes(), before);
}

#[test]
fn borrowed_entry_admission_does_not_reopen_the_lock_file() {
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let look = VaultLook::new(&fixture.paths);
    let before = crate::client::STATE_OBSERVATIONS.get();
    let result = snapshot
        .admit_in(&fixture.paths, &look, |_, reader| reader.observe("other"))
        .unwrap();
    assert!(matches!(
        result,
        EntryAdmission::Accepted(Ok(EntryRead::Present(_)))
    ));
    assert_eq!(crate::client::STATE_OBSERVATIONS.get(), before);
    drop(look);
    // The acquisition counter sees an ordinary admission taking its own guard.
    snapshot.admit(&fixture.paths, |_| ()).unwrap();
    assert!(crate::client::STATE_OBSERVATIONS.get() > before);
    assert!(writer_is_free(&fixture.paths));
}

#[test]
fn borrowed_entry_admission_checks_selected_source_and_reserved_names() {
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let look = VaultLook::new(&fixture.paths);
    let result = snapshot
        .admit_in(&fixture.paths, &look, |_, reader| {
            for name in [
                crate::commons::ENTRY,
                "aterm-claude-token-ac-0123456789abcdef",
            ] {
                assert!(reader.observe(name).is_err());
            }
        })
        .unwrap();
    assert!(matches!(result, EntryAdmission::Accepted(())));
    let mut unsigned = snapshot.clone();
    unsigned.config.signer = None;
    assert!(matches!(
        unsigned
            .admit_in(&fixture.paths, &look, |_, _| panic!(
                "unsigned source admitted"
            ))
            .unwrap(),
        EntryAdmission::Unavailable
    ));
    let mut wrong_recipient = snapshot.clone();
    wrong_recipient.recipient = Identity::from_secret_bytes([42; 32]).recipient();
    assert!(matches!(
        wrong_recipient
            .admit_in(&fixture.paths, &look, |_, _| panic!(
                "another recipient admitted"
            ))
            .unwrap(),
        EntryAdmission::Changed
    ));
    let mut reserved = snapshot.clone();
    reserved.name = crate::commons::ENTRY.into();
    assert!(
        reserved
            .admit_in(&fixture.paths, &look, |_, _| panic!(
                "reserved entry admitted"
            ))
            .is_err()
    );
    std::fs::remove_file(materialize::target_path(&fixture.paths.home, "token").unwrap()).unwrap();
    assert!(matches!(
        snapshot
            .admit_in(&fixture.paths, &look, |_, _| panic!(
                "missing materialization admitted"
            ))
            .unwrap(),
        EntryAdmission::Unavailable
    ));
}

#[test]
fn candidate_read_rechecks_the_lock_after_callback_entry() {
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let look = VaultLook::new(&fixture.paths);
    let result = snapshot
        .admit_in(&fixture.paths, &look, |_, reader| {
            assert!(matches!(
                reader.observe("other").unwrap(),
                EntryRead::Present(_)
            ));
            std::fs::rename(
                fixture.paths.lock_file(),
                fixture._root.path().join("old-lock"),
            )
            .unwrap();
            fsx::write_atomic(&fixture.paths.lock_file(), b"", 0o600).unwrap();
            reader.observe("other").unwrap()
        })
        .unwrap();
    assert!(matches!(
        result,
        EntryAdmission::Accepted(EntryRead::Unavailable)
    ));
    assert!(writer_is_free(&fixture.paths));
}

#[test]
fn candidate_read_requires_the_same_authenticated_version() {
    type Images = Vec<(PathBuf, Option<Vec<u8>>)>;
    let capture = |fixture: &Fixture| -> Images {
        fixture
            .tracked()
            .into_iter()
            .filter(|path| path != &fixture.paths.lock_file())
            .map(|path| {
                let bytes = std::fs::read(&path).ok();
                (path, bytes)
            })
            .collect()
    };
    let install = |images: &Images| {
        for (path, bytes) in images {
            if let Some(bytes) = bytes {
                std::fs::write(path, bytes).unwrap();
            } else if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
    };
    for mutant in [0, 4] {
        let fixture = Fixture::new();
        let before = capture(&fixture);
        fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "other", b"later-other")
                .unwrap();
        });
        let later = capture(&fixture);
        install(&before);
        let snapshot = fixture.read_token().unwrap().snapshot;
        let look = VaultLook::new(&fixture.paths);
        let _restore = Restore;
        MUTANT.set(mutant);
        let result = snapshot
            .admit_in(&fixture.paths, &look, |_, reader| {
                assert!(matches!(
                    reader.observe("other").unwrap(),
                    EntryRead::Present(_)
                ));
                // An out-of-protocol replay supplies a different, valid signed image.
                install(&later);
                reader.observe("other").unwrap()
            })
            .unwrap();
        MUTANT.set(0);
        assert_eq!(
            matches!(result, EntryAdmission::Accepted(EntryRead::Unavailable)),
            mutant == 0
        );
        drop(look);
        let observed = read_verified_entry(
            &fixture.paths,
            &config(&fixture.paths).unwrap().unwrap(),
            "other",
        )
        .unwrap()
        .unwrap();
        assert_eq!(observed.value().as_slice(), b"later-other");
    }
}
