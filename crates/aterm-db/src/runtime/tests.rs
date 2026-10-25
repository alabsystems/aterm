// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::fsx;

#[test]
fn missing_configuration_is_default_on_but_never_bootstraps_a_vault() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    assert!(enabled(&config).unwrap());
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(paths.clone(), config.clone(), move |s| {
        let _ = send.send(s);
    })
    .unwrap();
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(60)).unwrap(),
        Status::Dormant
    );
    runtime.stop();
    assert!(!paths.dir.exists());
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    assert!(!enabled(&config).unwrap());
    for source in ["[commons]\nenabled='false'", "commons=false", "[commons"] {
        fsx::write_atomic(&config, source.as_bytes(), 0o600).unwrap();
        assert!(enabled(&config).is_err());
    }
    fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    assert!(enabled(&config).unwrap());
}

#[test]
fn running_host_observes_exclusion_and_reenabling_without_restart() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let config = home.path().join("aterm.toml");
    let (send, receive) = std::sync::mpsc::channel();
    let runtime = Runtime::start(paths.clone(), config.clone(), move |status| {
        let _ = send.send(status);
    })
    .unwrap();
    let wait = |expected| {
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(60)).unwrap(),
            expected
        )
    };
    wait(Status::Dormant);
    fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    wait(Status::Disabled);
    fsx::write_atomic(&config, b"[commons]\nenabled=true\n", 0o600).unwrap();
    wait(Status::Dormant);
    runtime.stop();
    assert!(!paths.dir.exists());
}

fn lease_scenario(action: &str) -> bool {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("commons");
    let lease = owner::Lease::acquire(&dir).unwrap().unwrap();
    match action {
        "Acquire" => lease.recheck().is_ok(),
        "Contend" => owner::Lease::acquire(&dir).unwrap().is_some(),
        "Transfer" => {
            drop(lease);
            owner::Lease::acquire(&dir).unwrap().is_some()
        }
        "Replace" => {
            std::fs::rename(dir.join("runtime.lock"), dir.join("old.lock")).unwrap();
            fsx::write_new(&dir.join("runtime.lock"), b"", 0o600).unwrap();
            lease.recheck().is_ok()
        }
        "Remove" => {
            std::fs::remove_file(dir.join("runtime.lock")).unwrap();
            lease.recheck().is_ok()
        }
        _ => panic!("unknown lease action"),
    }
}

#[test]
fn actual_owner_lease_conforms_and_duplicate_owner_mutant_is_detected() {
    let model = aterm_models_db::transport_owner_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for (n, action) in ["Acquire", "Contend", "Transfer", "Replace", "Remove"]
        .iter()
        .enumerate()
    {
        let actual = lease_transition(&initial, action);
        assert_eq!(actual["scenario"], n as i64 + 1);
        assert!(
            model.successors(action, &initial).contains(&actual),
            "{action}: {actual:?}"
        );
    }
    let mut mutant = initial.clone();
    mutant.insert("scenario", 2);
    mutant.insert("usable", 1);
    assert!(!model.successors("Contend", &initial).contains(&mutant));
    assert!(!model.check_invariant("OnlyExclusiveIntactOwner", &mutant));
}

fn lease_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let scenario = match action {
        "Acquire" => 1,
        "Contend" => 2,
        "Transfer" => 3,
        "Replace" => 4,
        "Remove" => 5,
        _ => panic!("unknown lease action"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("usable", i64::from(lease_scenario(action)));
    next
}
