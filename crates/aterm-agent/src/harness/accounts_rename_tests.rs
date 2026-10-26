// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) const NOW: i64 = 1_790_000_000;

pub(crate) fn seed_reference(state: &mut State, logins: &mut Logins, label: &str, kind: i64) {
    match kind {
        0 => {
            for kind in [1, 3, 4, 5, 6, 7, 8, 9, 10] {
                seed_reference(state, logins, label, kind);
            }
        }
        1 | 2 => {
            logins.labels.insert(
                label.into(),
                Binding {
                    dir: (kind == 2).then(|| "/private/retained-login".into()),
                    renewed_at: Some(NOW - 1),
                    renewal_generation: 11,
                    subscription: Some("max".into()),
                    ..Binding::default()
                },
            );
        }
        3 => {
            state.accounts.insert(
                label.into(),
                Account {
                    auth: Auth::Ok,
                    auth_at: Some(NOW - 1),
                    ran_at: Some(NOW - 2),
                    ..Account::default()
                },
            );
        }
        4 => {
            state.active = Some(label.into());
        }
        5 => {
            state.previous = Some(label.into());
        }
        6 => {
            state.pending_pin = PinRequest::new(label, "a".repeat(32));
        }
        7 => {
            state.sessions.insert(
                format!("session-{label}"),
                SessionRow {
                    account: label.into(),
                    at: NOW,
                    held: Some("kept-reason".into()),
                },
            );
        }
        8 => {
            state.probe = Some(ProbeClaim {
                account: label.into(),
                sid: "probe-session".into(),
                at: NOW,
            });
        }
        9 => {
            state.reset = Some(ResetClaim {
                account: label.into(),
                until: Some(NOW + 3600),
                window: Some(WEEK_LIMIT.into()),
                sid: "reset-session".into(),
                at: NOW,
                pid: 42,
                start: "start-token".into(),
                done: None,
            });
        }
        10 => {
            let canary = Canary {
                sid: "canary-session".into(),
                at: NOW,
                took: false,
            };
            state.canary = Some(canary.clone());
            state.undo.push(Undo {
                previous: Some(label.into()),
                previous_proven: true,
                canary: Some(canary),
            });
        }
        _ => unreachable!(),
    }
}

pub(crate) fn rename_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Move" => {
            let case = state["case"];
            let mut current = State {
                pinned: true,
                previous_proven: true,
                ..State::default()
            };
            let mut logins = Logins::default();
            seed_reference(&mut current, &mut logins, "source", 0);
            if (2..=11).contains(&case) {
                seed_reference(&mut current, &mut logins, "destination", case - 1);
            }
            match case {
                22 => {
                    current = State::default();
                    logins = Logins::default();
                    seed_reference(&mut current, &mut logins, "destination", 3);
                }
                12 => {
                    for kind in [1, 3, 7] {
                        seed_reference(&mut current, &mut logins, "other", kind);
                    }
                }
                14 => {
                    current.accounts.clear();
                    logins.labels.clear();
                }
                15 => {
                    current = State::default();
                    logins = Logins::default();
                }
                18 => {
                    logins.labels.clear();
                }
                19 => {
                    current.accounts.clear();
                }
                20 => {
                    logins.generation_clock = u64::MAX;
                }
                _ => {}
            }
            let (from, to) = match case {
                13 => ("source", "source"),
                16 => ("invalid label", "destination"),
                17 => ("source", "invalid label"),
                21 => ("invalid label", "invalid label"),
                _ => ("source", "destination"),
            };
            let before = (current.clone(), logins.clone());
            let accepted = move_account(&mut current, &mut logins, from, to).is_ok();
            let changed = accepted && from != to;
            let preserved = changed || (&current, &logins) == (&before.0, &before.1);
            let payload = !changed
                || (current.accounts.get(to) == before.0.accounts.get(from)
                    && logins.labels.get(to) == before.1.labels.get(from));
            let detached = !changed
                || (!current.to_json().contains("\"source\"")
                    && !logins.to_json().contains("\"source\""));
            // In these fixtures the exact JSON string "source" occurs only as a label.
            // Substitution checks every other field, including nonce, claim IDs and history.
            let relabelled = |before: String, after: String| {
                aterm_json::from_str::<Value>(&after).unwrap()
                    == aterm_json::from_str::<Value>(
                        &before.replace("\"source\"", "\"destination\""),
                    )
                    .unwrap()
            };
            let unrelated = !changed
                || (relabelled(before.0.to_json(), current.to_json())
                    && relabelled(before.1.to_json(), logins.to_json()));
            next.insert("phase", 1);
            for (key, value) in [
                ("accepted", accepted),
                ("preserved", preserved),
                ("payload", payload),
                ("detached", detached),
                ("unrelated", unrelated),
            ] {
                next.insert(key, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_rename_conforms_to_retention_and_reference_model() {
    let model = aterm_models_harness::account_rename_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = rename_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let moved = rename_transition(&model.init_state(), "Move");
    for (field, law) in [
        ("payload", "PayloadPreserved"),
        ("detached", "NoStaleSourceReference"),
        ("unrelated", "UnrelatedStatePreserved"),
    ] {
        let mut forged = moved.clone();
        forged.insert(field, 0);
        assert!(!model.check_invariant(law, &forged));
    }
    let mut collision = model.init_state();
    collision.insert("case", 2);
    let mut refused = rename_transition(&collision, "Move");
    refused.insert("accepted", 1);
    assert!(!model.check_invariant("NoCollision", &refused));
    refused.insert("preserved", 0);
    assert!(!model.check_invariant("RefusalAndSelfMovePreserve", &refused));
}
