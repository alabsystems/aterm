// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::Fixture;
use super::*;
use aterm_spec::interp::State as ModelState;

const PROBE_HOME: &str = "ATERM_ENTRY_ADMISSION_PROBE_HOME";
const WRITER_FREE: &str = "admission-writer=free";
const WRITER_BUSY: &str = "admission-writer=busy";

#[test]
fn admission_writer_probe_child() {
    let Some(home) = std::env::var_os(PROBE_HOME) else {
        return;
    };
    let paths = Paths::for_home(Path::new(&home));
    let writer = StateLock::try_take(&paths).expect("writer probe");
    println!(
        "{}",
        if writer.is_some() {
            WRITER_FREE
        } else {
            WRITER_BUSY
        }
    );
}

/// Another process opens the real lock file and attempts the production
/// nonblocking exclusive acquisition. No scheduling or elapsed-time assertion.
fn writer_is_free(paths: &Paths) -> bool {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "client::entry::admission::tests::admission_writer_probe_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_HOME, &paths.home);
    let output = aterm_uds::deadline::output_until_complete_stdout(
        &mut child,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("writer probe completes");
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert_ne!(
        text.contains(WRITER_FREE),
        text.contains(WRITER_BUSY),
        "{text}"
    );
    text.contains(WRITER_FREE)
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    transition_with_early_release(state, action, false)
}

fn transition_with_early_release(
    state: &ModelState,
    action: &str,
    early_release: bool,
) -> ModelState {
    let mut next = state.clone();
    if action == "Case" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Admit");
    let fixture = Fixture::new();
    let mut snapshot = fixture.read_token().unwrap().snapshot;
    let mut writer = None;
    let token = materialize::target_path(&fixture.paths.home, "token").unwrap();
    match state["scenario"] {
        0 => {}
        1 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "other", b"updated-other")
                .unwrap();
        }),
        2 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "token", b"updated-token")
                .unwrap();
        }),
        3 => fixture.publish(|doc| {
            doc.unset_entry(&Audience::Everyone, "token").unwrap();
            doc.set_entry(
                &Audience::Hosts(vec!["admin".into(), "peer".into()]),
                "token",
                b"inert-authentic-token",
            )
            .unwrap();
        }),
        4 => snapshot.config.name = "different-vault".into(),
        5 => std::fs::remove_file(&token).unwrap(),
        6 => std::fs::remove_file(fixture.paths.lock_file()).unwrap(),
        7 => writer = Some(StateLock::take(&fixture.paths).unwrap()),
        8 => snapshot.name = crate::commons::ENTRY.into(),
        9 => snapshot.config.signer = None,
        _ => panic!("unknown admission case"),
    }
    let before = fixture.bytes();
    let marker = fixture._root.path().join("admitted-intent");
    let mut entered = false;
    let mut blocked = false;
    RELEASE_BEFORE_ADMISSION.set(early_release);
    let result = snapshot.admit(&fixture.paths, |entry| {
        entered = true;
        assert_eq!(entry.value().as_slice(), b"inert-authentic-token");
        fsx::write_atomic(&marker, b"prepared local intent", 0o600).unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), b"prepared local intent");
        // Probe after the real durability barrier, not only at callback entry.
        blocked = !writer_is_free(&fixture.paths);
    });
    RELEASE_BEFORE_ADMISSION.set(false);
    next.insert(
        "result",
        match result {
            Ok(EntryAdmission::Accepted(())) => 1,
            Ok(EntryAdmission::Busy) => 2,
            Ok(EntryAdmission::Unavailable) => 3,
            Ok(EntryAdmission::Changed) => 4,
            Err(_) => 5,
        },
    );
    next.insert("entered", i64::from(entered));
    next.insert("blocked", i64::from(blocked));
    next.insert("changed", i64::from(before != fixture.bytes()));
    assert_eq!(marker.exists(), entered);
    next.insert("released", i64::from(writer_is_free(&fixture.paths)));
    drop(writer);
    next.insert("done", 1);
    next
}

#[test]
fn admission_holds_the_vault_through_durable_intent_and_releases_on_return() {
    let model = aterm_models_db::entry_admission_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut admissions = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                admissions += usize::from(action.name == "Admit");
            }
        }
    }
    assert_eq!(admissions, 10);
    let bad = transition_with_early_release(&model.init_state(), "Admit", true);
    assert!(
        !model
            .successors("Admit", &model.init_state())
            .contains(&bad)
    );
    assert!(!model.check_invariant("HeldThroughDurability", &bad));
}

#[test]
fn callback_errors_and_unwinds_release_the_guard_without_replaying_the_callback() {
    let fixture = Fixture::new();
    let snapshot = fixture.read_token().unwrap().snapshot;
    let before = fixture.bytes();
    let mut calls = 0;
    let result = snapshot
        .admit(&fixture.paths, |_| {
            calls += 1;
            assert!(!writer_is_free(&fixture.paths));
            Err::<(), _>("application refused")
        })
        .unwrap();
    assert!(matches!(
        result,
        EntryAdmission::Accepted(Err("application refused"))
    ));
    assert_eq!(calls, 1);
    assert!(writer_is_free(&fixture.paths));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = snapshot.admit(&fixture.paths, |_| {
            calls += 1;
            assert!(!writer_is_free(&fixture.paths));
            panic!("application unwound");
        });
    }));
    assert!(result.is_err());
    assert_eq!(calls, 2);
    assert!(writer_is_free(&fixture.paths));
    assert_eq!(before, fixture.bytes());
}
