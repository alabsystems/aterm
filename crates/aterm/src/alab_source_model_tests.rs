// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

/// Exercise the real read transition, admitted cache observation and both consumer decisions.
/// Input actions select observations; they model no network or provider operation.
pub(crate) fn project(state: &State, action: &str) -> State {
    let mut next = state.clone();
    let input = match action {
        "Held" => Some("held"),
        "Read" => Some("read"),
        "Policy" => Some("policy"),
        "Project" => None,
        _ => panic!("unknown source-observation action"),
    };
    if let Some(input) = input {
        next.insert(input, state[input] + 1);
        return next;
    }

    let projected = |bound: bool| {
        if bound {
            tests::naming(Some("m3"))
        } else {
            LabRead::Projected(Box::default(), Inventory([8; 32]))
        }
    };
    let mut held = tests::nothing_held();
    if state["held"] > 0 {
        look(
            &mut held,
            Ok(Some(projected(state["held"] == 2))),
            false,
            &BTreeMap::new(),
            Instant::now(),
        );
    }
    let read = match state["read"] {
        0 => Ok(None),
        1 => Ok(Some(projected(false))),
        2 => Ok(Some(projected(true))),
        3 => Ok(Some(LabRead::Unchanged)),
        4 => Ok(Some(LabRead::Partial)),
        5 => Err("synthetic unavailable registry".to_owned()),
        _ => panic!("unknown registry observation"),
    };
    look(&mut held, read, false, &BTreeMap::new(), Instant::now());
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("aterm.toml");
    match state["policy"] {
        0 => {}
        1 => aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600).unwrap(),
        2 => aterm_uds::atomic_file::write(&config, b"[alab\n", 0o600).unwrap(),
        _ => panic!("unknown policy observation"),
    }
    let cache = Cache {
        paths: Paths::for_home(root.path()),
        config,
        held: Mutex::new(held),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    if state["policy"] > 0 {
        // Background refresh must not turn a policy exclusion into an absent registry.
        // Both branches return before opening registry storage.
        cache.refresh();
    }
    let retained = cache
        .held
        .lock()
        .unwrap()
        .lab
        .as_ref()
        .map_or(0, |lab| if lab.me.is_some() { 2 } else { 1 });
    let source = cache.projection();
    next.insert("retained", retained);
    next.insert("visible", i64::from(source.lab().is_some()));
    next.insert("pending", i64::from(source.pending()));
    next.insert("sheet", i64::from(source.permits_sheet()));
    next.insert(
        "naming",
        match source.machine_name() {
            RegistryNameRead::Unbound => 0,
            RegistryNameRead::Bound(_) => 1,
            RegistryNameRead::Pending => 2,
        },
    );
    next.insert("phase", 1);
    next
}

#[test]
fn source_availability_conforms_and_rejects_unknown_as_absent() {
    let model = aterm_models_harness::lab_source_availability_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut projected = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = project(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?}"
                );
                if action.name == "Project" {
                    projected += 1;
                }
            }
        }
    }
    assert_eq!(projected, 54);

    let mut unread = model.init_state();
    unread.insert("read", 5);
    let mut wrong = project(&unread, "Project");
    // The removed marker-dependent account rule allowed fallback while naming was pending.
    wrong.insert("pending", 0);
    wrong.insert("sheet", 1);
    assert!(!model.successors("Project", &unread).contains(&wrong));
    assert!(!model.check_invariant("PendingRead", &wrong));
    assert!(!model.check_invariant("SheetAdmission", &wrong));
}
