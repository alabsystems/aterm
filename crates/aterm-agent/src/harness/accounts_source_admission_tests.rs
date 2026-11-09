// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::alab_roster::{Lab, TestSourceOverride, test_machine};
use aterm_spec::interp::State;

pub(crate) fn resolve(state: &State, action: &str) -> State {
    resolve_with_origin(state, action, false)
}

fn resolve_with_origin(state: &State, action: &str, promote: bool) -> State {
    let mut next = state.clone();
    let input = match action {
        "Origin" => Some("origin"),
        "Change" => Some("change"),
        "Config" => Some("config"),
        "Resolve" => None,
        _ => panic!("unknown capture action"),
    };
    if let Some(input) = input {
        next.insert(input, state[input] + 1);
        return next;
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let roster = scratch.path().join("accounts.toml");
    let write = |bytes: &[u8]| aterm_uds::atomic_file::write(&config, bytes, 0o600).unwrap();
    write(b"[fleet]\nmachine = 'm16'\n");
    let initial = Arc::new(Lab {
        me: (state["origin"] == 2).then(|| test_machine("m16")),
        ..Lab::default()
    });
    let source = TestSourceOverride::new(SourceSnapshot::Present(initial.clone()));
    let captured = AutomaticSource::capture(&config).unwrap();
    source.set(match state["change"] {
        0 => SourceSnapshot::Present(initial.clone()),
        1 => SourceSnapshot::Present(Arc::new((*initial).clone())),
        2 => SourceSnapshot::Present(Arc::new(Lab {
            settled: true,
            ..(*initial).clone()
        })),
        3 => SourceSnapshot::Pending,
        4 => SourceSnapshot::Present(Arc::new(Lab {
            me: Some(test_machine("m27")),
            ..(*initial).clone()
        })),
        _ => panic!("unknown source change"),
    });
    match state["config"] {
        0 => {}
        1 => write(b"[fleet]\nmachine = 'm17'\n"),
        2 => write(b"[fleet]\nmachine = 'm16'\n[harness]\nenabled = false\n"),
        _ => panic!("unknown config change"),
    }
    let request = if state["origin"] == 0 || promote {
        SourceRequest::Requested(Some("captured.toml"))
    } else {
        SourceRequest::Automatic {
            candidate: Some("captured.toml"),
            admission: &captured,
        }
    };
    let selected = request.chosen(Cli::Claude, &roster, Some(&config));
    next.insert("admitted", i64::from(selected.is_ok()));
    next.insert("native", i64::from(selected.ok().flatten().is_some()));
    next.insert("phase", 1);
    next
}

#[test]
fn source_capture_conforms_and_rejects_implicit_promotion() {
    let model = aterm_models_harness::account_source_capture_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut resolved = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = resolve(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?}"
                );
                resolved += usize::from(action.name == "Resolve");
            }
        }
    }
    assert_eq!(resolved, 45);
    let mut pending = model.init_state();
    pending.insert("origin", 1);
    pending.insert("change", 3);
    let promoted = resolve_with_origin(&pending, "Resolve", true);
    assert!(!model.successors("Resolve", &pending).contains(&promoted));
    assert!(!model.check_invariant("SourceAdmission", &promoted));
}
