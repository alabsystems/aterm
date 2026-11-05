// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn limits(records: usize) -> Limits {
    Limits {
        records,
        bytes: records
            * (SUCCESSOR_BYTES + SUCCESSOR_SUBJECT.len() + aterm_journal::APPEND_OVERHEAD),
        record_bytes: SUCCESSOR_BYTES,
    }
}

fn candidate(root: &Prepare, level: usize, choice: i64) -> (Prepare, Binding) {
    let index = 10 + level * 2 + usize::try_from(choice).unwrap();
    let mut prepare = root.clone();
    prepare.id = format!("{index:032x}");
    let binding = Binding {
        id: prepare.id.clone(),
        capability: format!("{index:064x}"),
    };
    (prepare, binding)
}

fn event(paths: &Paths, pair: &(Prepare, Binding), event: Event, cap: Limits) {
    record_event_at_limits(paths, &pair.0, &pair.1, Some(event), cap).unwrap();
}

fn claim(
    paths: &Paths,
    parent: &(Prepare, Binding),
    child: &(Prepare, Binding),
    cap: Limits,
) -> io::Result<Observed> {
    record_successor_at(paths, &parent.0, &parent.1, &child.0, &child.1, cap)
}

fn project(
    paths: &Paths,
    root_pair: &(Prepare, Binding),
    state: &aterm_spec::interp::State,
) -> aterm_spec::interp::State {
    let (key, view) = read(paths).unwrap().unwrap();
    let root = &root_pair.0;
    let mut next = state.clone();
    let mut entry = &view.entries[&key.operation(&root.id)];
    let mut depth = 0;
    let mut unsafe_parent = false;
    let mut changed = entry.receipt != receipt(&key, root, &root_pair.1).unwrap();
    for name in ["one", "two", "three"] {
        let Some(child) = entry.successor else {
            next.insert(name, 0);
            continue;
        };
        unsafe_parent |= entry.state != IssuerState::NoExec;
        let choice = [1, 2]
            .into_iter()
            .find(|choice| key.operation(&candidate(root, depth, *choice).0.id) == child)
            .unwrap();
        next.insert(name, choice);
        let parent = entry.receipt.operation;
        entry = &view.entries[&child];
        assert_eq!(entry.predecessor, Some(parent));
        let expected = candidate(root, depth, choice);
        changed |= entry.receipt != receipt(&key, &expected.0, &expected.1).unwrap();
        depth += 1;
    }
    next.insert("depth", depth as i64);
    next.insert("tail", issuer_number(entry.state));
    next.insert("records", view.sequence as i64);
    next.insert("bytes", view.committed_bytes as i64);
    next.insert("unsafe_parent", i64::from(unsafe_parent));
    next.insert(
        "changed",
        i64::from(changed || view.entries.len() != depth + 1),
    );
    next.insert("authority", 0);
    next
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (_home, paths, root, binding) = fixture();
    let cap = Limits {
        bytes: state["byte_limit"] as usize,
        ..limits(state["slots"] as usize)
    };
    let root_pair = (root.clone(), binding);
    let mut chain = vec![root_pair.clone()];
    event(&paths, &chain[0], Event::Prepare, cap);
    for (level, name) in ["one", "two", "three"]
        .into_iter()
        .enumerate()
        .take(state["depth"] as usize)
    {
        let parent = chain.last().unwrap();
        event(&paths, parent, Event::NoExec, cap);
        let child = candidate(&root, level, state[name]);
        claim(&paths, parent, &child, cap).unwrap();
        chain.push(child);
    }
    let current = chain.last().unwrap();
    if state["tail"] == 2 || state["tail"] == 5 {
        event(&paths, current, Event::MayExecute, cap);
    }
    match state["tail"] {
        3 => event(&paths, current, Event::NoExec, cap),
        4 | 5 => event(&paths, current, Event::Seal, cap),
        _ => {}
    }
    assert_eq!(
        project(&paths, &root_pair, state),
        *state,
        "reconstructed storage"
    );
    let mut next = state.clone();
    match action {
        "Room7" => {
            next.insert("slots", 7);
            next.insert("byte_limit", 3633);
        }
        "Room9" => {
            next.insert("slots", 9);
            next.insert("byte_limit", 4671);
        }
        "TightBytes" => {
            next.insert("byte_limit", 2420);
        }
        "May" => event(&paths, current, Event::MayExecute, cap),
        "NoExec" => event(&paths, current, Event::NoExec, cap),
        "Seal" => event(&paths, current, Event::Seal, cap),
        "ClaimA" | "ClaimB" | "ClaimLostAck" => {
            let choice = if action == "ClaimB" { 2 } else { 1 };
            let child = candidate(&root, state["depth"] as usize, choice);
            claim(&paths, current, &child, cap).unwrap();
            if action != "ClaimLostAck" {
                next.insert("ack", state["depth"] + 1);
            }
        }
        "Retry" | "RetryClosed" | "Rival" | "Conflict" => {
            let parent = &chain[chain.len() - 2];
            let mut child = current.clone();
            if action == "Rival" {
                let choice = state[["one", "two", "three"][chain.len() - 2]];
                child = candidate(&root, chain.len() - 2, 3 - choice);
            } else if action == "Conflict" {
                child.0.deadline_us += 1;
            }
            assert_eq!(
                claim(&paths, parent, &child, cap).is_ok(),
                action == "Retry"
            );
            if action == "Retry" {
                next.insert("ack", state["depth"]);
            }
        }
        "FailedClaim" => {
            // A caller with the wrong predecessor capability gets no partial claim.
            let mut parent = current.clone();
            parent.1.capability = "dd".repeat(32);
            let child = candidate(&root, state["depth"] as usize, 1);
            assert!(claim(&paths, &parent, &child, cap).is_err());
        }
        "Reopen" => {
            read(&paths).unwrap().unwrap();
        }
        _ => panic!("unknown successor action {action}"),
    }
    project(&paths, &root_pair, &next)
}

#[test]
fn atomic_successors_refine_three_link_races_retries_and_capacity() {
    let model = aterm_models_harness::noexec_successor_model();
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "successor model audit: {faults:?}");
    aterm_spec::interp::prove_and_catch(&model);
    let states = aterm_spec::interp::reachable_states(&model);
    assert!(states.iter().any(|state| state["depth"] == 3));
    for state in states {
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
    // Negative implementation: publish success after separately preparing the
    // candidate, without consuming its predecessor in the same durable record.
    let (_home, paths, root, binding) = fixture();
    let cap = limits(5);
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, cap);
    event(&paths, &parent, Event::NoExec, cap);
    let child = candidate(&root, 0, 1);
    event(&paths, &child, Event::Prepare, cap);
    let mut bad = project(&paths, &parent, &model.init_state());
    bad.insert("ack", 1);
    assert!(!model.check_invariant("Acknowledged", &bad));
    assert!(!model.check_invariant("Counted", &bad));
    // Storage still sees the predecessor as unused and admits a second child.
    assert!(claim(&paths, &parent, &candidate(&root, 0, 2), limits(9)).is_ok());
}

#[test]
fn claims_require_exact_live_epoch_noexec_and_preserve_all_links() {
    for state in [0, 1, 2, 4, 5] {
        let (_home, paths, root, binding) = fixture();
        let parent = (root.clone(), binding);
        if state == 0 {
            record_at(&paths, &parent.0, &parent.1).unwrap();
        } else {
            event(&paths, &parent, Event::Prepare, LIMITS);
        }
        if state == 2 || state == 5 {
            event(&paths, &parent, Event::MayExecute, LIMITS);
        }
        if state == 4 || state == 5 {
            event(&paths, &parent, Event::Seal, LIMITS);
        }
        assert!(claim(&paths, &parent, &candidate(&root, 0, 1), LIMITS).is_err());
    }
    let (_home, paths, root, binding) = fixture();
    let mut parent = (root.clone(), binding);
    let root_pair = parent.clone();
    event(&paths, &parent, Event::Prepare, LIMITS);
    for level in 0..3 {
        event(&paths, &parent, Event::NoExec, LIMITS);
        let child = candidate(&root, level, 1);
        let mut wrong_epoch = child.clone();
        wrong_epoch.0.host_epoch = "ee".repeat(16);
        assert!(claim(&paths, &parent, &wrong_epoch, LIMITS).is_err());
        let mut wrong_recipe = parent.clone();
        wrong_recipe.0.deadline_us += 1;
        assert!(claim(&paths, &wrong_recipe, &child, LIMITS).is_err());
        claim(&paths, &parent, &child, LIMITS).unwrap();
        assert!(claim(&paths, &parent, &candidate(&root, level, 2), LIMITS).is_err());
        parent = child;
    }
    event(&paths, &parent, Event::MayExecute, LIMITS);
    event(&paths, &parent, Event::Seal, LIMITS);
    let mut state = aterm_models_harness::noexec_successor_model().init_state();
    for (name, value) in [
        ("slots", 9),
        ("byte_limit", 4671),
        ("depth", 3),
        ("tail", 5),
        ("one", 1),
        ("two", 1),
        ("three", 1),
        ("records", 9),
        ("bytes", 4461),
        ("ack", 3),
    ] {
        state.insert(name, value);
    }
    assert_eq!(project(&paths, &root_pair, &state), state);
}

#[test]
fn concurrent_successor_claims_choose_one_and_lost_reply_retries_without_appending() {
    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, LIMITS);
    event(&paths, &parent, Event::NoExec, LIMITS);
    let children = [candidate(&root, 0, 1), candidate(&root, 0, 2)];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_paths = paths.clone();
    let worker_parent = parent.clone();
    let worker_child = children[0].clone();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        claim(&worker_paths, &worker_parent, &worker_child, LIMITS)
    });
    barrier.wait();
    let second = claim(&paths, &parent, &children[1], LIMITS);
    assert_ne!(worker.join().unwrap().is_ok(), second.is_ok());
    let (key, view) = read(&paths).unwrap().unwrap();
    assert_eq!(view.sequence, 3);
    let winner = view.entries[&key.operation(&root.id)].successor.unwrap();
    let child = children
        .iter()
        .find(|child| key.operation(&child.0.id) == winner)
        .unwrap();
    let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
    claim(&paths, &parent, child, LIMITS).unwrap();
    assert_eq!(
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
        before
    );
    for secret in [
        &root.id,
        &parent.1.capability,
        "private-conversation",
        "do-not-write-this-value",
    ] {
        assert!(
            !before
                .windows(secret.len())
                .any(|value| value == secret.as_bytes())
        );
    }
}

#[test]
fn rejected_capacity_does_not_consume_parent_and_closing_slots_are_reserved() {
    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, LIMITS);
    event(&paths, &parent, Event::NoExec, LIMITS);
    let child = candidate(&root, 0, 1);
    assert!(claim(&paths, &parent, &child, limits(4)).is_err());
    let (key, view) = read(&paths).unwrap().unwrap();
    assert_eq!(view.sequence, 2);
    assert!(view.entries[&key.operation(&root.id)].successor.is_none());
    assert!(!view.entries.contains_key(&key.operation(&child.0.id)));
    claim(&paths, &parent, &child, limits(5)).unwrap();
    event(&paths, &child, Event::MayExecute, limits(5));
    event(&paths, &child, Event::Seal, limits(5));
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 5);
    assert!(claim(&paths, &parent, &child, limits(5)).is_err());
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 5);
}

#[test]
fn successor_frames_authenticate_lineage_and_reject_reordered_or_forked_claims() {
    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, LIMITS);
    event(&paths, &parent, Event::NoExec, LIMITS);
    let (key, mut view) = read(&paths).unwrap().unwrap();
    let child = candidate(&root, 0, 1);
    let epoch = key.digest(b"epoch\0", root.host_epoch.as_bytes());
    let frame = view.encode_successor(
        &key,
        &receipt(&key, &child.0, &child.1).unwrap(),
        epoch,
        key.operation(&root.id),
        17,
    );
    for index in 0..frame.len() {
        let mut bad = frame.clone();
        bad[index] ^= 1;
        assert!(view.ingest(&key, &bad).is_err(), "tamper byte {index}");
    }
    assert!(View::default().ingest(&key, &frame).is_err());
    view.ingest(&key, &frame).unwrap();
    let before_sequence = view.sequence;
    let rival = candidate(&root, 0, 2);
    let fork = view.encode_successor(
        &key,
        &receipt(&key, &rival.0, &rival.1).unwrap(),
        epoch,
        key.operation(&root.id),
        18,
    );
    assert!(view.ingest(&key, &fork).is_err());
    assert_eq!(view.sequence, before_sequence);
    assert_eq!(
        view.entries[&key.operation(&root.id)].successor,
        Some(key.operation(&child.0.id))
    );
    assert!(!view.entries.contains_key(&key.operation(&rival.0.id)));
}

#[test]
fn exact_byte_reservations_keep_old_full_logs_readable_and_reserve_larger_claims() {
    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    let old_full = Limits {
        records: 3,
        bytes: 3 * ISSUER_FRAME,
        record_bytes: SUCCESSOR_BYTES,
    };
    event(&paths, &parent, Event::Prepare, old_full);
    event(&paths, &parent, Event::MayExecute, old_full);
    event(&paths, &parent, Event::Seal, old_full);
    let key = write_key(&paths).unwrap();
    let (_, view) = open(&paths, &key, old_full).unwrap();
    assert_eq!(view.committed_bytes, 3 * ISSUER_FRAME);
    assert!(view.within_capacity(old_full, 0));

    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, LIMITS);
    event(&paths, &parent, Event::NoExec, LIMITS);
    let child = candidate(&root, 0, 1);
    let exact = Limits {
        records: 5,
        bytes: 4 * ISSUER_FRAME + SUCCESSOR_FRAME,
        record_bytes: SUCCESSOR_BYTES,
    };
    assert!(
        claim(
            &paths,
            &parent,
            &child,
            Limits {
                bytes: exact.bytes - 1,
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        claim(
            &paths,
            &parent,
            &child,
            Limits {
                bytes: 5 * ISSUER_FRAME,
                ..exact
            }
        )
        .is_err()
    );
    assert!(
        read(&paths)
            .unwrap()
            .unwrap()
            .1
            .entries
            .values()
            .all(|entry| entry.successor.is_none())
    );
    claim(&paths, &parent, &child, exact).unwrap();
    event(&paths, &child, Event::MayExecute, exact);
    event(&paths, &child, Event::Seal, exact);
    assert_eq!(
        read(&paths).unwrap().unwrap().1.committed_bytes,
        exact.bytes
    );
}

#[test]
fn ordinary_and_successor_preparation_replays_do_not_alias() {
    let (_home, paths, root, binding) = fixture();
    let parent = (root.clone(), binding);
    event(&paths, &parent, Event::Prepare, LIMITS);
    event(&paths, &parent, Event::NoExec, LIMITS);
    let plain = candidate(&root, 0, 1);
    event(&paths, &plain, Event::Prepare, LIMITS);
    assert!(claim(&paths, &parent, &plain, LIMITS).is_err());
    let successor = candidate(&root, 0, 2);
    claim(&paths, &parent, &successor, LIMITS).unwrap();
    assert!(record_event_at(&paths, &successor.0, &successor.1, Some(Event::Prepare)).is_err());
    event(&paths, &successor, Event::NoExec, LIMITS);
    assert!(claim(&paths, &parent, &successor, LIMITS).is_err());
}
