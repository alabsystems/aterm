// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn renew(store: &Store, label: &str, at: i64) -> Result<Vec<Event>, String> {
    let dir = store.logins_dir().join(label);
    store.transact_logins(at, &mut None, |state, logins| {
        Ok(note_renewed(
            state,
            logins,
            label,
            dir.to_str(),
            dir.to_str().unwrap(),
            None,
            at,
        ))
    })
}

pub(crate) fn generation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Commit" => {
            let dir = aterm_tempfile::tempdir().unwrap();
            let store = Store::at(dir.path());
            let case = state["case"];
            if case > 1 {
                renew(&store, "work", NOW).unwrap();
            }
            if matches!(case, 6 | 14) {
                let (mut logins, _) = store.load_logins(NOW).unwrap();
                logins.labels.get_mut("work").unwrap().renewal_generation = u64::MAX;
                logins.generation_clock = u64::MAX;
                store.save_logins(&logins).unwrap();
            }
            let ever_issued = store.load_logins(NOW).unwrap().0.generation_clock;
            if case == 10 {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        logins.labels.remove("work");
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            let (before, _) = store.load_logins(NOW).unwrap();
            let prior = before
                .get("work")
                .map_or(0, |binding| binding.renewal_generation);
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let bytes = files.each_ref().map(|path| std::fs::read(path).ok());
            let result = match case {
                4 => renew(&store, "other", NOW),
                5 => store.transact_logins(NOW, &mut None, |state, logins| {
                    let path = store.logins_dir().join("work");
                    note_renewed(
                        state,
                        logins,
                        "work",
                        path.to_str(),
                        path.to_str().unwrap(),
                        None,
                        NOW,
                    );
                    Err("the caller refused publication".into())
                }),
                7 => store.transact_logins(NOW, &mut None, |state, logins| {
                    let path = store.logins_dir().join("work");
                    let mut events = note_renewed(
                        state,
                        logins,
                        "work",
                        path.to_str(),
                        path.to_str().unwrap(),
                        None,
                        NOW,
                    );
                    events.extend(note_renewed(
                        state,
                        logins,
                        "work",
                        path.to_str(),
                        path.to_str().unwrap(),
                        None,
                        NOW,
                    ));
                    Ok(events)
                }),
                8 => store.transact_logins(NOW + 1, &mut None, |_, logins| {
                    note_served_login(logins, "work", NOW + 1);
                    Ok(Vec::new())
                }),
                9 => store.transact_logins(NOW, &mut None, |state, logins| {
                    logins.labels.remove("work");
                    state.active = Some("other".into());
                    Ok(vec![Event::new(NOW, Kind::Renewed, "work")])
                }),
                11 => store.transact_logins(NOW, &mut None, |state, logins| {
                    move_account(state, logins, "work", "moved")?;
                    let path = store.logins_dir().join("work");
                    Ok(note_renewed(
                        state,
                        logins,
                        "work",
                        path.to_str(),
                        path.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                }),
                12 => store.transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.get_mut("work").unwrap().dir = Some(
                        store
                            .logins_dir()
                            .join("replacement")
                            .to_str()
                            .unwrap()
                            .into(),
                    );
                    Ok(Vec::new())
                }),
                13 => store.transact_logins(NOW, &mut None, |_, logins| {
                    note_identity(logins, "work", Proof::Status, Some("owner@example.test"));
                    Ok(Vec::new())
                }),
                14 => store.transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.remove("work");
                    Ok(Vec::new())
                }),
                15 => store.transact_logins(NOW, &mut None, |state, logins| {
                    let dir = store.logins_dir().join("work");
                    let events = note_renewed(
                        state,
                        logins,
                        "work",
                        dir.to_str(),
                        dir.to_str().unwrap(),
                        None,
                        NOW,
                    );
                    logins.labels.get_mut("work").unwrap().dir = None;
                    Ok(events)
                }),
                _ => renew(&store, "work", if case == 3 { NOW - 10 } else { NOW }),
            };
            let (after, _) = store.load_logins(NOW + 1).unwrap();
            let current = after
                .get("work")
                .map_or(0, |binding| binding.renewal_generation);
            let advanced = current > ever_issued;
            if advanced && case != 11 {
                assert_eq!(before.generation_clock.checked_add(1), Some(current));
            }
            if case == 4 {
                assert_eq!(after.get("other").unwrap().renewal_generation, 2);
            }
            if case == 11 {
                assert!(after.get("moved").unwrap().renewal_generation > prior);
                assert_eq!(after.generation_clock, before.generation_clock + 2);
            }
            next.insert("phase", 2);
            next.insert("accepted", i64::from(result.is_ok()));
            next.insert("advanced", i64::from(advanced));
            next.insert(
                "preserved",
                i64::from(
                    result.is_ok()
                        || bytes == files.each_ref().map(|path| std::fs::read(path).ok()),
                ),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn committed_renewal_generations_conform_without_clock_assumptions() {
    let model = aterm_models_harness::account_renewal_generation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = generation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut repeated = model.init_state();
    repeated.insert("case", 2);
    repeated.insert("phase", 1);
    let mut forged = generation_transition(&repeated, "Commit");
    forged.insert("advanced", 0);
    assert!(!model.check_invariant("RenewalAdvances", &forged));
    repeated.insert("case", 6);
    let mut forged = generation_transition(&repeated, "Commit");
    forged.insert("accepted", 1);
    assert!(!model.check_invariant("Refused", &forged));
    repeated.insert("case", 3);
    let mut forged = generation_transition(&repeated, "Commit");
    assert_eq!(
        forged["accepted"], 1,
        "a renewal under a clock set back publishes"
    );
    forged.insert("accepted", 0);
    assert!(!model.check_invariant("AllowedPublishes", &forged));
}

#[test]
fn generation_is_durable_and_bad_or_legacy_encodings_cannot_wrap_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());
    renew(&store, "work", NOW).unwrap();
    renew(&store, "work", NOW).unwrap();
    let (logins, _) = store.load_logins(NOW).unwrap();
    assert_eq!(logins.get("work").unwrap().renewal_generation, 2);
    assert_eq!(Logins::from_json(&logins.to_json(), NOW).unwrap(), logins);
    let mut legacy: Value = aterm_json::from_str(&logins.to_json()).unwrap();
    assert_eq!(
        legacy.get("schema").and_then(Value::as_u64),
        Some(LOGINS_SCHEMA)
    );
    legacy
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(2));
    assert_eq!(Logins::from_json(&legacy.to_string(), NOW).unwrap(), logins);
    let work = legacy
        .as_object_mut()
        .unwrap()
        .get_mut("labels")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("work")
        .unwrap()
        .as_object_mut()
        .unwrap();
    work.remove("renewal_generation");
    assert!(Logins::from_json(&aterm_json::to_string(&legacy).unwrap(), NOW).is_err());
    legacy
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(1u64));
    legacy.as_object_mut().unwrap().remove("generation_clock");
    assert_eq!(
        Logins::from_json(&aterm_json::to_string(&legacy).unwrap(), NOW)
            .unwrap()
            .get("work")
            .unwrap()
            .renewal_generation,
        0
    );
    for bad in [
        Value::Null,
        Value::from(-1i64),
        Value::from("18446744073709551616"),
        Value::from(1.5),
    ] {
        let mut broken: Value = aterm_json::from_str(&logins.to_json()).unwrap();
        broken
            .as_object_mut()
            .unwrap()
            .get_mut("labels")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("renewal_generation".into(), bad);
        assert!(Logins::from_json(&aterm_json::to_string(&broken).unwrap(), NOW).is_err());
    }
    for bad in [Value::from(0u64), Value::Null, Value::from("2")] {
        let mut broken: Value = aterm_json::from_str(&logins.to_json()).unwrap();
        broken
            .as_object_mut()
            .unwrap()
            .insert("generation_clock".into(), bad);
        assert!(Logins::from_json(&aterm_json::to_string(&broken).unwrap(), NOW).is_err());
    }
}

#[test]
fn the_last_generation_is_usable_and_exhaustion_aborts_every_binding_in_the_edit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());
    renew(&store, "work", NOW).unwrap();
    renew(&store, "z-last", NOW).unwrap();
    let (mut logins, _) = store.load_logins(NOW).unwrap();
    logins.labels.get_mut("z-last").unwrap().renewal_generation = u64::MAX - 1;
    logins.generation_clock = u64::MAX - 1;
    store.save_logins(&logins).unwrap();
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let result = store.transact_logins(NOW, &mut None, |state, logins| {
        let mut events = Vec::new();
        for label in ["work", "z-last"] {
            let path = store.logins_dir().join(label);
            events.extend(note_renewed(
                state,
                logins,
                label,
                path.to_str(),
                path.to_str().unwrap(),
                None,
                NOW,
            ));
        }
        Ok(events)
    });
    assert!(result.is_err());
    assert_eq!(
        before,
        files.each_ref().map(|path| std::fs::read(path).ok())
    );
    renew(&store, "z-last", NOW).unwrap();
    assert_eq!(store.load_logins(NOW).unwrap().0.generation_clock, u64::MAX);
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    assert!(renew(&store, "work", NOW).is_err());
    assert_eq!(
        before,
        files.each_ref().map(|path| std::fs::read(path).ok())
    );
}
