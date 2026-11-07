// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::contract::Scope;
use crate::harness::native_act::{Image, Process, RestartCommand};
use crate::harness::upgrade::Dialect;
use std::os::unix::fs::PermissionsExt;

#[path = "native_successor_tests.rs"]
mod successor;

#[path = "native_receipts_phase_tests.rs"]
mod phase;

#[path = "native_receipts_account_tests.rs"]
mod accounts;

fn fixture() -> (aterm_tempfile::TempDir, Paths, Prepare, Binding) {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let image = |n| Image {
        path: format!("/private/image-{n}").into_bytes(),
        device: 1,
        inode: n,
    };
    let process = |pid| Process {
        pid,
        birth: 70,
        parent: 1,
        group: pid,
        session: 400,
        image: image(u64::from(pid)),
    };
    let prepare = Prepare {
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "claude-code@900".into(),
        revision: 1,
        expected_human_seq: Some(7),
        deadline_us: 1_000_000,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            cold: None,
            process: process(500),
            shell: process(400),
            launcher: image(20),
            observation: None,
            socket: "/private/native.sock".into(),
            forward: Box::new(RestartCommand {
                settles: None,
                account_intent: None,
                image: image(90),
                program: "/private/image-90".into(),
                argv: vec!["--resume".into(), "private-conversation".into()],
                env: vec![("PRIVATE_CANARY".into(), "do-not-write-this-value".into())],
                unset: None,
                cwd: None,
                heal: None,
                dialect: Dialect::Zsh,
            }),
            rollback: None,
        },
    };
    let binding = Binding {
        id: prepare.id.clone(),
        capability: "ef".repeat(32),
    };
    assert!(Request::prepare(prepare.clone()).encode().is_some());
    (home, paths, prepare, binding)
}

#[test]
fn acknowledged_noexec_survives_reopen_and_exact_retry_appends_nothing() {
    let (_home, paths, prepare, binding) = fixture();
    assert_eq!(observe_at(&paths, &binding).unwrap(), None);
    assert!(
        !paths.dir.exists(),
        "an observation creates no identity or directory"
    );
    let observed = record_at(&paths, &prepare, &binding).unwrap();
    let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
    assert_eq!(observe_at(&paths, &binding).unwrap(), Some(observed));
    assert_eq!(
        observe_prepare_at(&paths, &prepare).unwrap(),
        Some(observed)
    );
    assert_eq!(record_at(&paths, &prepare, &binding).unwrap(), observed);
    assert_eq!(
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
        before
    );
    for private in [
        "private-conversation",
        "do-not-write-this-value",
        &binding.capability,
        &prepare.id,
    ] {
        assert!(
            !before
                .windows(private.len())
                .any(|bytes| bytes == private.as_bytes())
        );
    }
    let mut wrong = binding.clone();
    wrong.capability = "de".repeat(32);
    assert_eq!(observe_at(&paths, &wrong).unwrap(), None);
    wrong = binding.clone();
    wrong.id = "ee".repeat(16);
    assert_eq!(observe_at(&paths, &wrong).unwrap(), None);
    assert!(record_at(&paths, &prepare, &wrong).is_err());
    let mut changed = prepare.clone();
    changed.deadline_us += 1;
    assert_eq!(observe_prepare_at(&paths, &changed).unwrap(), None);
    assert!(record_at(&paths, &changed, &binding).is_err());
    assert_eq!(
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
        before
    );
}

#[test]
fn lost_enrollment_after_key_capture_cannot_reenroll_an_existing_only_open() {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    let key = existing_key(&paths).unwrap();
    std::fs::rename(directory(&paths), paths.dir.join("saved-journal")).unwrap();
    std::fs::rename(
        paths.dir.join(ENROLLMENT),
        paths.dir.join("saved-enrollment"),
    )
    .unwrap();
    assert!(open_existing(&paths, &key, LIMITS).is_err());
    assert!(!directory(&paths).exists());
    assert!(!paths.dir.join(ENROLLMENT).exists());
}

#[test]
fn concurrent_local_writers_preserve_one_immutable_receipt() {
    let (_home, paths, prepare, binding) = fixture();
    aterm_secrets::client::machine_identity(&paths).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let a_gate = std::sync::Arc::clone(&gate);
    let a_paths = paths.clone();
    let a_prepare = prepare.clone();
    let a_binding = binding.clone();
    let worker = std::thread::spawn(move || {
        a_gate.wait();
        record_at(&a_paths, &a_prepare, &a_binding)
    });
    gate.wait();
    let answers = [
        record_at(&paths, &prepare, &binding),
        worker.join().unwrap(),
    ];
    let observed: Vec<Observed> = answers
        .into_iter()
        .filter_map(|answer| match answer {
            Ok(observed) => Some(observed),
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                None
            }
        })
        .collect();
    assert!(!observed.is_empty(), "one writer must record the receipt");
    assert!(observed.iter().all(|answer| *answer == observed[0]));
    assert_eq!(observe_at(&paths, &binding).unwrap(), Some(observed[0]));
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 1);
}

#[test]
fn lost_or_changed_established_custody_is_never_fresh_authority() {
    for damage in [
        "checkpoint",
        "astream",
        "anchor",
        "enrollment",
        "identity",
        "missing-identity",
        "permissions",
    ] {
        let (_home, paths, prepare, binding) = fixture();
        record_at(&paths, &prepare, &binding).unwrap();
        match damage {
            "enrollment" => std::fs::rename(
                paths.dir.join(ENROLLMENT),
                paths.dir.join("saved-enrollment"),
            )
            .unwrap(),
            "identity" => std::fs::write(paths.dir.join("identity"), b"corrupt").unwrap(),
            "missing-identity" => {
                std::fs::rename(paths.dir.join("identity"), paths.dir.join("saved-identity"))
                    .unwrap()
            }
            "permissions" => {
                std::fs::set_permissions(directory(&paths), std::fs::Permissions::from_mode(0o755))
                    .unwrap()
            }
            name => std::fs::rename(
                directory(&paths).join(format!("noexec.{name}")),
                directory(&paths).join(format!("saved-{name}")),
            )
            .unwrap(),
        }
        assert!(record_at(&paths, &prepare, &binding).is_err(), "{damage}");
        if damage == "missing-identity" {
            assert!(
                !paths.dir.join("identity").exists(),
                "a failed receipt must not replace the missing key"
            );
        }
        assert!(observe_at(&paths, &binding).is_err(), "{damage}");
        assert!(observe_prepare_at(&paths, &prepare).is_err(), "{damage}");
    }
}

#[test]
fn authenticated_sequence_rejects_tampering_duplicates_and_reordering() {
    let identity = Identity::from_secret_bytes([7; 32]);
    let key = Key::new(&identity);
    let other = Key::new(&Identity::from_secret_bytes([8; 32]));
    let receipt = Receipt {
        operation: [1; 32],
        prepare: [2; 32],
        capability: [3; 32],
    };
    let first = View::default().encode(&key, &receipt, 11);
    let mut view = View::default();
    view.ingest(&key, &first).unwrap();
    assert!(view.ingest(&key, &first).is_err());
    let next = Receipt {
        operation: [4; 32],
        ..receipt
    };
    let second = view.encode(&key, &next, 12);
    assert!(View::default().ingest(&key, &second).is_err());
    assert!(View::default().ingest(&other, &first).is_err());
    for i in 0..first.len() {
        let mut tampered = first.clone();
        tampered[i] ^= 1;
        assert!(View::default().ingest(&key, &tampered).is_err(), "byte {i}");
    }
    view.ingest(&key, &second).unwrap();
    assert_eq!(view.entries.len(), 2);
}

fn other_epoch(prepare: &Prepare, binding: &Binding) -> (Prepare, Binding) {
    let mut other = prepare.clone();
    other.id = "91".repeat(16);
    other.host_epoch = "92".repeat(16);
    let mut other_binding = binding.clone();
    other_binding.id.clone_from(&other.id);
    other_binding.capability = "93".repeat(32);
    (other, other_binding)
}

fn five_records() -> Limits {
    Limits {
        records: 5,
        bytes: 5 * (SUCCESSOR_BYTES + SUCCESSOR_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD),
        record_bytes: SUCCESSOR_BYTES,
    }
}

#[test]
fn issuer_history_keeps_may_execute_after_sealing_and_never_observes_it_as_noexec() {
    let (_home, paths, prepare, binding) = fixture();
    let prepared = record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    assert_eq!(observe_at(&paths, &binding).unwrap(), None);
    assert_eq!(
        record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap(),
        prepared
    );
    let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
    let mut different = prepare.clone();
    different.deadline_us += 1;
    assert!(record_event_at(&paths, &different, &binding, Some(Event::MayExecute)).is_err());
    assert_eq!(
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
        before
    );
    record_event_at(&paths, &prepare, &binding, Some(Event::MayExecute)).unwrap();
    assert!(record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).is_err());
    assert!(record_at(&paths, &prepare, &binding).is_err());
    record_event_at(&paths, &prepare, &binding, Some(Event::Seal)).unwrap();
    let (key, view) = read(&paths).unwrap().unwrap();
    assert_eq!(
        view.entries[&key.operation(&prepare.id)].state,
        IssuerState::Sealed { may_execute: true }
    );
    assert_eq!(view.sequence, 3);
    assert_eq!(observe_at(&paths, &binding).unwrap(), None);
    assert_eq!(observe_prepare_at(&paths, &prepare).unwrap(), None);
    assert!(record_event_at(&paths, &prepare, &binding, Some(Event::MayExecute)).is_err());
    record_event_at(&paths, &prepare, &binding, Some(Event::Seal)).unwrap();
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 3);
}

#[test]
fn closing_capacity_counts_two_live_epochs_and_releases_only_after_a_terminal_record() {
    let (_home, paths, a, a_binding) = fixture();
    let (b, b_binding) = other_epoch(&a, &a_binding);
    let put = |prepare: &Prepare, binding: &Binding, event| {
        record_event_at_limits(&paths, prepare, binding, Some(event), five_records())
    };
    put(&a, &a_binding, Event::Prepare).unwrap();
    // Counting only B's epoch would admit this and promise six records in five slots.
    assert!(put(&b, &b_binding, Event::Prepare).is_err());
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 1);
    put(&a, &a_binding, Event::NoExec).unwrap();
    assert!(observe_at(&paths, &a_binding).unwrap().is_some());
    put(&b, &b_binding, Event::Prepare).unwrap();
    put(&b, &b_binding, Event::MayExecute).unwrap();
    put(&b, &b_binding, Event::Seal).unwrap();
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 5);
    let mut c = a.clone();
    c.id = "94".repeat(16);
    let mut c_binding = a_binding.clone();
    c_binding.id.clone_from(&c.id);
    assert!(put(&c, &c_binding, Event::Prepare).is_err());
    assert!(observe_at(&paths, &a_binding).unwrap().is_some());
}

#[test]
fn concurrent_epoch_preparations_cannot_spend_the_same_closing_slots() {
    let (_home, paths, a, a_binding) = fixture();
    let (b, b_binding) = other_epoch(&a, &a_binding);
    aterm_secrets::client::machine_identity(&paths).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_gate = gate.clone();
    let worker_paths = paths.clone();
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        record_event_at_limits(
            &worker_paths,
            &a,
            &a_binding,
            Some(Event::Prepare),
            five_records(),
        )
    });
    gate.wait();
    let second =
        record_event_at_limits(&paths, &b, &b_binding, Some(Event::Prepare), five_records());
    assert_ne!(worker.join().unwrap().is_ok(), second.is_ok());
    let view = read(&paths).unwrap().unwrap().1;
    assert_eq!(view.sequence, 1);
    assert!(view.within_capacity(five_records(), 0));
}

#[test]
fn legacy_receipts_survive_an_issuer_chain_and_new_frames_authenticate_their_kind_and_epoch() {
    let (_home, paths, a, a_binding) = fixture();
    let old = record_at(&paths, &a, &a_binding).unwrap();
    let (b, b_binding) = other_epoch(&a, &a_binding);
    record_event_at(&paths, &b, &b_binding, Some(Event::Prepare)).unwrap();
    record_event_at(&paths, &b, &b_binding, Some(Event::NoExec)).unwrap();
    assert_eq!(observe_at(&paths, &a_binding).unwrap(), Some(old));
    assert!(observe_at(&paths, &b_binding).unwrap().is_some());
    assert!(record_event_at(&paths, &a, &a_binding, Some(Event::Prepare)).is_err());
    assert_eq!(
        record_event_at(&paths, &a, &a_binding, Some(Event::NoExec)).unwrap(),
        old
    );
    let key = Key::new(&Identity::from_secret_bytes([7; 32]));
    let receipt = Receipt {
        operation: [1; 32],
        prepare: [2; 32],
        capability: [3; 32],
    };
    let body = View::default().encode_event(&key, &receipt, [4; 32], Event::Prepare, 17);
    for at in 0..body.len() {
        let mut bad = body.clone();
        bad[at] ^= 1;
        assert!(View::default().ingest(&key, &bad).is_err(), "byte {at}");
    }
    let mut view = View::default();
    view.ingest(&key, &body).unwrap();
    assert!(
        view.ingest(&key, &body).is_err(),
        "duplicate wire records are not retries"
    );
}

fn issuer_number(state: IssuerState) -> i64 {
    match state {
        IssuerState::Prepared => 1,
        IssuerState::MayExecute => 2,
        IssuerState::NoExec => 3,
        IssuerState::Sealed { may_execute: false } => 4,
        IssuerState::Sealed { may_execute: true } => 5,
        IssuerState::Fenced => 6,
    }
}

pub(crate) fn issuer_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (_home, paths, a, a_binding) = fixture();
    let (b, b_binding) = other_epoch(&a, &a_binding);
    let actors = [(&a, &a_binding, state["a"]), (&b, &b_binding, state["b"])];
    let put = |prepare, binding, event| {
        record_event_at_limits(&paths, prepare, binding, Some(event), five_records())
    };
    // Reconstruct closed prefixes before outstanding reservations. A two-record
    // closure can fit before a three-record closure, but not in reverse order.
    let mut seed = actors.to_vec();
    seed.sort_by_key(|(_, _, phase)| match phase {
        3 | 4 => 0,
        5 => 1,
        _ => 2,
    });
    for (prepare, binding, phase) in seed {
        if phase == 0 {
            continue;
        }
        put(prepare, binding, Event::Prepare).unwrap();
        if phase == 2 || phase == 5 {
            put(prepare, binding, Event::MayExecute).unwrap();
        }
        if phase == 3 {
            put(prepare, binding, Event::NoExec).unwrap();
        }
        if phase == 4 || phase == 5 {
            put(prepare, binding, Event::Seal).unwrap();
        }
    }
    let mut next = state.clone();
    match action {
        "PrepareA" => {
            let _ = put(&a, &a_binding, Event::Prepare);
        }
        "PrepareB" => {
            let _ = put(&b, &b_binding, Event::Prepare);
        }
        "MayA" => {
            put(&a, &a_binding, Event::MayExecute).unwrap();
        }
        "MayB" => {
            put(&b, &b_binding, Event::MayExecute).unwrap();
        }
        "NoExecA" => {
            put(&a, &a_binding, Event::NoExec).unwrap();
        }
        "NoExecB" => {
            put(&b, &b_binding, Event::NoExec).unwrap();
        }
        "SealA" => {
            put(&a, &a_binding, Event::Seal).unwrap();
        }
        "SealB" => {
            put(&b, &b_binding, Event::Seal).unwrap();
        }
        "ObserveA" => {
            next.insert(
                "observed",
                i64::from(observe_at(&paths, &a_binding).unwrap().is_some()),
            );
        }
        "RetryA" | "ConflictA" => {
            let event = match state["a"] {
                1 => Event::Prepare,
                2 => Event::MayExecute,
                3 => Event::NoExec,
                _ => Event::Seal,
            };
            let mut requested = a.clone();
            if action == "ConflictA" {
                requested.deadline_us += 1;
            }
            let result = put(&requested, &a_binding, event);
            assert_eq!(result.is_ok(), action == "RetryA");
        }
        "ReopenA" => {
            assert!(put(&a, &a_binding, Event::Prepare).is_err());
        }
        _ => panic!("unknown issuer action {action}"),
    }
    let loaded = read(&paths).unwrap();
    for (name, prepare) in [("a", &a), ("b", &b)] {
        let phase = loaded
            .as_ref()
            .and_then(|(key, view)| view.entries.get(&key.operation(&prepare.id)))
            .map_or(0, |entry| issuer_number(entry.state));
        next.insert(name, phase);
        next.insert(
            if name == "a" { "a_may" } else { "b_may" },
            i64::from(phase == 2 || phase == 5),
        );
        next.insert(
            if name == "a" { "a_closed" } else { "b_closed" },
            i64::from(phase >= 3),
        );
    }
    next.insert(
        "records",
        loaded.as_ref().map_or(0, |(_, view)| view.sequence as i64),
    );
    next
}

#[test]
fn issuer_journal_refines_capacity_and_sticky_execution_model() {
    let model = aterm_models_harness::issuer_journal_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = issuer_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    // Negative implementation: append the second epoch's preparation through
    // the real log without the application's reservation check. The raw log's
    // record cap alone accepts two records, which owe six records of capacity.
    let (_home, paths, a, a_binding) = fixture();
    let (b, b_binding) = other_epoch(&a, &a_binding);
    record_event_at_limits(&paths, &a, &a_binding, Some(Event::Prepare), five_records()).unwrap();
    let key = write_key(&paths).unwrap();
    let (mut log, view) = open(&paths, &key, five_records()).unwrap();
    let receipt = Receipt {
        operation: key.operation(&b.id),
        prepare: key.prepare(&b).unwrap(),
        capability: key.capability(&b_binding),
    };
    let body = view.encode_event(
        &key,
        &receipt,
        key.digest(b"epoch\0", b.host_epoch.as_bytes()),
        Event::Prepare,
        20,
    );
    log.append(ISSUER_SUBJECT, body).unwrap();
    drop(log);
    let bad = read(&paths).unwrap().unwrap().1;
    assert!(!bad.within_capacity(five_records(), 0));
    let mut projected = model.init_state();
    projected.insert("a", 1);
    projected.insert("b", 1);
    projected.insert("records", bad.sequence as i64);
    assert!(!model.check_invariant("Capacity", &projected));
    assert!(
        open(&paths, &key, five_records()).is_err(),
        "an overcommitted prefix is never adopted"
    );
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (_home, paths, prepare, original) = fixture();
    aterm_secrets::client::machine_identity(&paths).unwrap();
    if state["stored"] == 1 {
        record_at(&paths, &prepare, &original).unwrap();
    }
    let damage = || {
        if state["stored"] == 1 {
            std::fs::write(directory(&paths).join("noexec.checkpoint"), [0u8; 40]).unwrap();
        }
    };
    if state["healthy"] == 0 {
        damage();
    }
    let lose_key = || {
        let changed = Identity::from_secret_bytes([8; 32]).to_bech32();
        std::fs::write(paths.dir.join("identity"), changed.as_slice()).unwrap();
    };
    if state["key"] == 0 {
        lose_key();
    }
    let mut binding = original.clone();
    if state["same"] == 0 {
        binding.capability = "dc".repeat(32);
    }
    let mut next = state.clone();
    match action {
        "Record" | "Retry" => {
            record_at(&paths, &prepare, &binding).unwrap();
            next.insert("stored", 1);
            next.insert(
                "records",
                i64::try_from(read(&paths).unwrap().unwrap().1.sequence).unwrap(),
            );
        }
        "Conflict" => {
            let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
            let accepted = record_at(&paths, &prepare, &binding).is_ok();
            let after = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
            next.insert("changed", i64::from(accepted || before != after));
        }
        "Damage" => {
            damage();
            next.insert("healthy", 0);
        }
        "LoseKey" => {
            lose_key();
            next.insert("key", 0);
        }
        "ChangeBinding" => {
            next.insert("same", 0);
        }
        "Read" => {
            let returned = observe_at(&paths, &binding).ok().flatten().is_some();
            next.insert("read", 1);
            next.insert("returned", i64::from(returned));
            next.insert("absent_read", i64::from(returned && state["stored"] == 0));
            next.insert("damaged_read", i64::from(returned && state["healthy"] == 0));
            next.insert("wrong_key_read", i64::from(returned && state["key"] == 0));
            next.insert(
                "wrong_binding_read",
                i64::from(returned && state["same"] == 0),
            );
        }
        _ => panic!("unknown model action {action}"),
    }
    next
}

#[test]
fn real_journal_refines_bounded_receipt_observation_and_rejects_forged_results() {
    let model = aterm_models_harness::noexec_receipt_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let (_home, paths, prepare, mut binding) = fixture();
    record_at(&paths, &prepare, &binding).unwrap();
    binding.capability = "dc".repeat(32);
    assert_eq!(observe_at(&paths, &binding).unwrap(), None);
    // Negative implementation: treating the presence of a receipt file as a
    // positive answer, without authenticating the caller's exact capability.
    let unchecked = directory(&paths).join("noexec.astream").exists();
    let mut forged = model.init_state();
    forged.insert("wrong_binding_read", i64::from(unchecked));
    assert!(!model.check_invariant("NeedsExactBinding", &forged));
}
